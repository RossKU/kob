package org.kob.executor

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import android.os.PowerManager
import android.os.SystemClock
import java.io.File
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Runs `kobd` (the roles of kob-executor.toml) as a foreground service: a persistent notification and a wake lock
 * while it runs. If the process exits it is started again after a growing pause; the user stops it with ACTION_STOP.
 * Before the first start of a service run, a database of an earlier build is brought to the current schema
 * (`kobd db migrate`); if that fails, the service stops and says why instead of restarting. The previous store the update
 * keeps beside the new one is deleted once kobd has run on the new one ([dropOldStoreOnceProven]).
 */
class ExecutorService : Service() {
    companion object {
        const val ACTION_STOP = "org.kob.executor.STOP"
        const val PREFS = "kob"
        const val PREF_RUNNING = "running"

        private const val CHANNEL = "executor"
        private const val NOTE_ID = 1
        private const val LOG_LINES = 300
        private const val BACKOFF_MIN_MS = 5_000L
        private const val BACKOFF_MAX_MS = 300_000L
        /** How long kobd must have run on a migrated store, its read API reporting the store followed, before the old one goes. */
        private const val PROVEN_AFTER_MS = 10 * 60_000L
        private const val PROVE_POLL_MS = 30_000L

        /** stopped, updating (the database update before the start), running or restarting (waiting to start again after an exit). */
        @Volatile var state = "stopped"
        /** Why the last start was given up (a failed database update) and what to do; null when it was not. */
        @Volatile var failure: String? = null
        @Volatile var updatingSinceMs = 0L
        @Volatile var startedAtMs = 0L
        @Volatile var restarts = 0
        @Volatile var retryAtMs = 0L
        @Volatile var lastExit = ""

        private val log = ArrayDeque<String>()

        fun push(line: String) = synchronized(log) {
            if (log.size >= LOG_LINES) log.removeFirst()
            log.addLast(line)
        }

        fun lastLines(n: Int): List<String> = synchronized(log) { log.toList().takeLast(n) }
    }

    @Volatile private var wanted = false
    @Volatile private var process: Process? = null
    private var supervisor: Thread? = null
    private var stopLatch = CountDownLatch(1)
    private var wakeLock: PowerManager.WakeLock? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val prefs = getSharedPreferences(PREFS, Context.MODE_PRIVATE)
        if (intent?.action == ACTION_STOP) {
            shutdown()
            return START_NOT_STICKY
        }
        // restarted by the system after the app's process was killed: continue only if the user had it running
        if (intent == null && !prefs.getBoolean(PREF_RUNNING, false)) {
            stopSelf()
            return START_NOT_STICKY
        }
        try {
            enterForeground()
        } catch (e: Exception) {
            // the system refused a foreground service from the background (a restart while the phone was idle)
            push("cannot run in the foreground: $e")
            stopSelf()
            return START_NOT_STICKY
        }
        prefs.edit().putBoolean(PREF_RUNNING, true).apply()
        if (supervisor?.isAlive != true) {
            wanted = true
            failure = null
            stopLatch = CountDownLatch(1)
            restarts = 0
            acquireWakeLock()
            state = "running"
            val t = Thread({ supervise(stopLatch) }, "kob-supervisor")
            supervisor = t
            t.start()
        }
        return START_STICKY
    }

    override fun onDestroy() {
        wanted = false
        stopLatch.countDown()
        process?.destroy()
        releaseWakeLock()
        super.onDestroy()
    }

    private fun shutdown() {
        getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(PREF_RUNNING, false).apply()
        wanted = false
        stopLatch.countDown()
        process?.destroy()
        val t = supervisor
        Thread({
            // SIGTERM first; whatever is still there after a few seconds is killed
            t?.join(4_000)
            Kob.killStale(this)
            t?.join(2_000)
            releaseWakeLock()
            state = "stopped"
            stopForeground(STOP_FOREGROUND_REMOVE)
            stopSelf()
        }, "kob-stop").start()
    }

    /** The database update, then the executor, again and again until [wanted] is cleared. */
    private fun supervise(latch: CountDownLatch) {
        val why = updateDatabase()
        if (why != null && wanted) {
            giveUp(why)
            return
        }
        var backoff = BACKOFF_MIN_MS
        while (wanted) {
            Kob.killStale(this)
            val began = SystemClock.elapsedRealtime()
            try {
                // no arguments: kob-executor.toml of KOB_EXECUTOR_DIR, the same as `kobd --executor|--indexer --config ...`
                val p = Kob.command(this).redirectErrorStream(true).start()
                process = p
                p.outputStream.close()
                startedAtMs = System.currentTimeMillis()
                state = "running"
                notify("Running")
                if (oldStore().any { it.exists() }) {
                    Thread({ dropOldStoreOnceProven(p, began, latch) }, "kob-old-store").apply { isDaemon = true }.start()
                }
                p.inputStream.bufferedReader().forEachLine { push(it) }
                lastExit = "exit code ${p.waitFor()}"
            } catch (e: Exception) {
                lastExit = e.toString()
            }
            process = null
            push("executor ended ($lastExit)")
            if (!wanted) break
            if (SystemClock.elapsedRealtime() - began > 10 * 60_000L) backoff = BACKOFF_MIN_MS
            restarts++
            state = "restarting"
            retryAtMs = System.currentTimeMillis() + backoff
            notify("Stopped unexpectedly, starting again in ${backoff / 1000} s")
            if (latch.await(backoff, TimeUnit.MILLISECONDS)) break
            backoff = minOf(backoff * 2, BACKOFF_MAX_MS)
        }
        state = "stopped"
    }

    /** The previous store a database update keeps (`index.sqlite3.before-migrate`), with the -wal and -shm files that belong to it. */
    private fun oldStore(): List<File> {
        val old = File(Kob.dir(this), "kob-data/index.sqlite3.before-migrate")
        return listOf(old, File(old.path + "-wal"), File(old.path + "-shm"))
    }

    /**
     * Deletes the previous store once the migrated one has proven itself in the kobd process [p] (started at [began],
     * elapsedRealtime): its read API has reported the indexer following the chain on the full store (state following or
     * catching_up, book_complete not false: the store opened, is read and written) and the same process has then run for
     * [PROVEN_AFTER_MS]. An exit, a restart or a stop before that keeps the old file, and the next start watches again: the old
     * store is never deleted while the new one has not run. (`kobd db migrate` also refuses a later update while it is there.)
     */
    private fun dropOldStoreOnceProven(p: Process, began: Long, latch: CountDownLatch) {
        var followed = false
        while (wanted && p.isAlive && process === p) {
            if (!followed) {
                followed = runCatching {
                    val h = Kob.health()
                    h.optString("state") in setOf("following", "catching_up") && h.optBoolean("book_complete", true)
                }.getOrDefault(false)
            }
            if (followed && SystemClock.elapsedRealtime() - began >= PROVEN_AFTER_MS) {
                if (!p.isAlive || process !== p) return
                val gone = oldStore().filter { it.exists() }.all { it.delete() }
                push(
                    if (gone) "the previous database (index.sqlite3.before-migrate) is deleted: the updated one has run for ${PROVEN_AFTER_MS / 60_000} min"
                    else "the previous database (index.sqlite3.before-migrate) could not be deleted; it is tried again at the next start"
                )
                return
            }
            if (latch.await(PROVE_POLL_MS, TimeUnit.MILLISECONDS)) return
        }
    }

    /**
     * Brings a database written by an earlier build (the v0.1.0 app wrote schema 5) to the schema this kobd reads:
     * `kobd db --config <dir>/kob-executor.toml migrate`. It builds the new store beside the old one, verifies it row by row
     * and only then swaps it in (the old file stays as index.sqlite3.before-migrate until [dropOldStoreOnceProven] deletes it);
     * the key file is not touched. A store of the current schema is left as it is (a moment); without a store (a first start)
     * nothing runs. Returns null when the start may go on, else what failed and what to do.
     */
    private fun updateDatabase(): String? {
        val db = File(Kob.dir(this), "kob-data/index.sqlite3")
        if (!db.isFile) return null
        Kob.killStale(this)
        // the new store is about 3/4 of the old one, plus the temporary files of the index builds
        val need = db.length() + File(db.path + "-wal").length()
        val free = db.parentFile?.usableSpace ?: Long.MAX_VALUE
        if (free < need) {
            return "not enough free storage to check and update the database (${free shr 20} MB free, about ${need shr 20} MB " +
                "needed). Free up space on the phone, then switch Run on again."
        }
        updatingSinceMs = System.currentTimeMillis()
        state = "updating"
        notify("Updating the database")
        push("checking the database (kobd db migrate) ...")
        var error: String? = null
        var last = ""
        val code = try {
            val cfg = File(Kob.dir(this), "kob-executor.toml").path
            val p = Kob.command(this, "db", "--config", cfg, "migrate").redirectErrorStream(true).start()
            process = p
            p.outputStream.close()
            p.inputStream.bufferedReader().forEachLine {
                push(it)
                if (it.startsWith("Error:")) error = it.removePrefix("Error:").trim()
                if (it.isNotBlank()) last = it
            }
            p.waitFor()
        } catch (e: Exception) {
            error = e.toString()
            -1
        } finally {
            process = null
        }
        if (code == 0 || !wanted) return null
        val what = (error ?: last.ifBlank { "exit code $code" }).take(400)
        return "the database could not be updated: $what. The old database is unchanged until an update succeeds. Switch Run " +
            "on to try again; if it fails again, copy the log and report it."
    }

    /** Stops the service after a failed database update: no restart loop; the main screen shows [failure]. */
    private fun giveUp(why: String) {
        failure = why
        push("not started: $why")
        getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(PREF_RUNNING, false).apply()
        wanted = false
        state = "stopped"
        releaseWakeLock()
        stopForeground(STOP_FOREGROUND_REMOVE)
        getSystemService(NotificationManager::class.java)
            .notify(NOTE_ID, notification("Not started: the database could not be updated. Open the app.", ongoing = false))
        stopSelf()
    }

    private fun enterForeground() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            val nm = getSystemService(NotificationManager::class.java)
            nm.createNotificationChannel(NotificationChannel(CHANNEL, "KOB executor", NotificationManager.IMPORTANCE_LOW))
        }
        val n = notification("Starting")
        when {
            // dataSync services are limited to 6 hours a day from Android 15: specialUse has no limit
            Build.VERSION.SDK_INT >= 35 -> startForeground(NOTE_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_SPECIAL_USE)
            Build.VERSION.SDK_INT >= 29 -> startForeground(NOTE_ID, n, ServiceInfo.FOREGROUND_SERVICE_TYPE_DATA_SYNC)
            else -> startForeground(NOTE_ID, n)
        }
    }

    @Suppress("DEPRECATION") // Notification.Builder(Context) below API 26 (no channels)
    private fun notification(text: String, ongoing: Boolean = true): Notification {
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val b = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) Notification.Builder(this, CHANNEL) else Notification.Builder(this)
        return b.setContentTitle("KOB executor")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentIntent(open)
            .setOngoing(ongoing)
            .build()
    }

    private fun notify(text: String) {
        getSystemService(NotificationManager::class.java).notify(NOTE_ID, notification(text))
    }

    private fun acquireWakeLock() {
        if (wakeLock?.isHeld == true) return
        val pm = getSystemService(PowerManager::class.java)
        wakeLock = pm.newWakeLock(PowerManager.PARTIAL_WAKE_LOCK, "kob:executor").apply {
            setReferenceCounted(false)
            acquire()
        }
    }

    private fun releaseWakeLock() {
        wakeLock?.let { if (it.isHeld) it.release() }
        wakeLock = null
    }
}

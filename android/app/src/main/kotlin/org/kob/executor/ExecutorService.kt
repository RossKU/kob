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
import java.util.concurrent.CountDownLatch
import java.util.concurrent.TimeUnit

/**
 * Runs `kob-executor` (the role of kob-executor.toml) as a foreground service: a persistent notification and a wake lock
 * while it runs. If the process exits it is started again after a growing pause; the user stops it with ACTION_STOP.
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

        /** stopped, running or restarting (waiting to start again after an exit). */
        @Volatile var state = "stopped"
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

    /** The executor, again and again until [wanted] is cleared. */
    private fun supervise(latch: CountDownLatch) {
        var backoff = BACKOFF_MIN_MS
        while (wanted) {
            Kob.killStale(this)
            val began = SystemClock.elapsedRealtime()
            try {
                // no arguments: kob-executor.toml of KOB_EXECUTOR_DIR, the same as `kob-executor run|index --config ...`
                val p = Kob.command(this).redirectErrorStream(true).start()
                process = p
                p.outputStream.close()
                startedAtMs = System.currentTimeMillis()
                state = "running"
                notify("Running")
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
    private fun notification(text: String): Notification {
        val open = PendingIntent.getActivity(this, 0, Intent(this, MainActivity::class.java), PendingIntent.FLAG_IMMUTABLE)
        val b = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) Notification.Builder(this, CHANNEL) else Notification.Builder(this)
        return b.setContentTitle("KOB executor")
            .setContentText(text)
            .setSmallIcon(android.R.drawable.stat_notify_sync)
            .setContentIntent(open)
            .setOngoing(true)
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

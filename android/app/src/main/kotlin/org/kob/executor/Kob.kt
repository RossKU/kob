package org.kob.executor

import android.content.Context
import java.io.File

/** Where the executor binary and its files are, and how to run it. */
object Kob {
    /** The executor's read API (its default listen address, `[api]` of kob-executor.toml). */
    const val API = "http://127.0.0.1:8090"

    /** The matcher's low-funds threshold (kob-executor `MIN_FUNDS_KAS`). */
    const val MIN_FUNDS_KAS = 100.0

    /** The operator key file in [dir] (64 hex characters; `kob-executor setup` writes it). Its contents never leave the app except by the user's own export. */
    const val KEY_FILE = "kob-operator.key"

    /** Configuration, key file and database: the app's private storage (no other app can read it). */
    fun dir(ctx: Context): File = File(ctx.filesDir, "kob").also { it.mkdirs() }

    /** The binary is shipped as a native library so that it is executable (W^X: not from the data directory). */
    fun binary(ctx: Context): File = File(ctx.applicationInfo.nativeLibraryDir, "libkobexecutor.so")

    /** The executor with `args`; without arguments it runs kob-executor.toml of [dir] (what the service does). */
    fun command(ctx: Context, vararg args: String): ProcessBuilder {
        val tmp = File(ctx.cacheDir, "tmp").also { it.mkdirs() }
        val pb = ProcessBuilder(listOf(binary(ctx).path) + args)
        pb.directory(dir(ctx))
        pb.environment().apply {
            put("KOB_EXECUTOR_DIR", dir(ctx).path)
            put("HOME", ctx.filesDir.path)
            put("TMPDIR", tmp.path)
            put("NO_COLOR", "1")
        }
        return pb
    }

    /** What a finished subcommand left: its exit code, stdout, and stderr (the executor's log and, on failure, its error). */
    class Output(val code: Int, val out: String, val err: String)

    /**
     * Runs a short subcommand to its end and returns the exit code and both streams; throws only after [timeoutMs] (the process
     * is killed). Several may run at once (each has its own stderr file). Never call it on the UI thread.
     */
    fun exec(ctx: Context, timeoutMs: Long, vararg args: String): Output {
        val err = File.createTempFile("cmd", ".err", ctx.cacheDir)
        try {
            val p = command(ctx, *args).redirectError(err).start()
            p.outputStream.close()
            var out = ""
            val reader = Thread { out = runCatching { p.inputStream.bufferedReader().readText() }.getOrDefault("") }
            reader.start()
            reader.join(timeoutMs)
            if (reader.isAlive) {
                p.destroyForcibly()
                throw RuntimeException("timed out after ${timeoutMs / 1000} s (no answer from the node)")
            }
            val code = p.waitFor()
            return Output(code, out, runCatching { err.readText() }.getOrDefault(""))
        } finally {
            err.delete()
        }
    }

    /**
     * Runs a short subcommand and returns its stdout (stderr, the log, is dropped); throws on failure or after [timeoutMs]
     * with the last line of stderr. Never call it on the UI thread.
     */
    fun run(ctx: Context, timeoutMs: Long, vararg args: String): String {
        val o = exec(ctx, timeoutMs, *args)
        if (o.code != 0) {
            val why = o.err.trim().lines().lastOrNull { it.isNotBlank() }
            throw RuntimeException(why ?: if (o.code == 137) "killed (exit code 137)" else "exit code ${o.code}")
        }
        return o.out
    }

    /**
     * Kills executor processes of this app left over from an earlier run of the app (they would hold the database). Only the
     * long-running ones: the service starts the binary without arguments. A short subcommand (`address-balance`, `setup`)
     * has arguments and is left alone; it used to be killed here too, which showed as "exit code 137" (SIGKILL) whenever the
     * service started the executor, or restarted it, while the balance was being read.
     */
    fun killStale(ctx: Context) {
        for (pid in longRunning(ctx)) android.os.Process.killProcess(pid)
    }

    /** True while a long-running executor process of this app exists (a withdrawal needs the database's writer lock). */
    fun running(ctx: Context): Boolean = longRunning(ctx).isNotEmpty()

    private fun longRunning(ctx: Context): List<Int> {
        val me = android.os.Process.myPid()
        val bin = binary(ctx).path
        val found = mutableListOf<Int>()
        File("/proc").listFiles()?.forEach { d ->
            val pid = d.name.toIntOrNull() ?: return@forEach
            if (pid == me) return@forEach
            try {
                val argv = File(d, "cmdline").readText().split('\u0000').filter { it.isNotEmpty() }
                if (argv.size == 1 && argv[0] == bin) found += pid
            } catch (_: Exception) {
            }
        }
        return found
    }
}

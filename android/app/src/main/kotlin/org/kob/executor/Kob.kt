package org.kob.executor

import android.content.Context
import java.io.File

/** Where the executor binary and its files are, and how to run it. */
object Kob {
    /** The executor's read API (its default listen address, `[api]` of kob-executor.toml). */
    const val API = "http://127.0.0.1:8090"

    /** The matcher's low-funds threshold (kob-executor `MIN_FUNDS_KAS`). */
    const val MIN_FUNDS_KAS = 100.0

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

    /** Runs a short subcommand and returns its stdout (stderr, the log, is dropped); throws on failure or after [timeoutMs]. */
    fun run(ctx: Context, timeoutMs: Long, vararg args: String): String {
        val err = File(ctx.cacheDir, "cmd.err")
        val p = command(ctx, *args).redirectError(err).start()
        p.outputStream.close()
        var out = ""
        val reader = Thread { out = p.inputStream.bufferedReader().readText() }
        reader.start()
        reader.join(timeoutMs)
        if (reader.isAlive) {
            p.destroy()
            throw RuntimeException("timed out after ${timeoutMs / 1000} s")
        }
        val code = p.waitFor()
        if (code != 0) {
            val why = runCatching { err.readText().trim().lines().lastOrNull { it.isNotBlank() } }.getOrNull()
            throw RuntimeException(why ?: "exit code $code")
        }
        return out
    }

    /** Kills executor processes of this app left over from an earlier run of the app (they would hold the database). */
    fun killStale(ctx: Context) {
        val me = android.os.Process.myPid()
        val bin = binary(ctx).path
        File("/proc").listFiles()?.forEach { d ->
            val pid = d.name.toIntOrNull() ?: return@forEach
            if (pid == me) return@forEach
            try {
                if (File(d, "cmdline").readText().startsWith(bin)) android.os.Process.killProcess(pid)
            } catch (_: Exception) {
            }
        }
    }
}

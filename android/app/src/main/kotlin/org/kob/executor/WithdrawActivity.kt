package org.kob.executor

import android.app.Activity
import android.app.AlertDialog
import android.content.Context
import android.content.Intent
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.text.InputType
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import java.util.concurrent.Executors

/**
 * Sends KAS out of the operator key: an address, an amount in KAS or "all", and a confirmation. The executor holds the
 * database's single-writer lock while it runs, so it is stopped first (the service, then the process); then
 * `kob-executor withdraw --to ... --amount ... --yes` runs over the node of the setup, the result is shown, and the executor
 * can be started again from here.
 */
class WithdrawActivity : Activity() {
    private val ui = Handler(Looper.getMainLooper())
    private val work = Executors.newSingleThreadExecutor()
    private lateinit var fromText: TextView
    private lateinit var toEdit: EditText
    private lateinit var amountEdit: EditText
    private lateinit var allButton: Button
    private lateinit var sendButton: Button
    private lateinit var resultText: TextView
    private lateinit var restartButton: Button

    @Volatile private var busy = false
    private var wasRunning = false

    private companion object {
        val AMOUNT = Regex("""[0-9]+(\.[0-9]{1,8})?""")
        const val STOP_WAIT_MS = 20_000L
        const val WITHDRAW_TIMEOUT_MS = 120_000L
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val address = getSharedPreferences(ExecutorService.PREFS, Context.MODE_PRIVATE).getString("address", "") ?: ""
        setContentView(
            page { root ->
                root.addView(label("Withdraw", 22f, bold = true))
                root.addView(label("From this address (the executor key):", 13f))
                fromText = label(address.ifEmpty { "(not set up)" }, 13f, mono = true, selectable = true)
                root.addView(fromText)
                root.addView(label("To the address:", 13f))
                toEdit = textField("kaspatest:...", mono = true, inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS)
                root.addView(toEdit)
                root.addView(label("Amount in KAS, or all:", 13f))
                amountEdit = textField("e.g. 25.5 or all", inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_NO_SUGGESTIONS)
                root.addView(amountEdit)
                allButton = button("Send everything (all)") { amountEdit.setText("all") }
                root.addView(allButton)
                root.addView(
                    label(
                        "The executor is stopped first, because it uses the funds. The transaction is final within seconds and " +
                            "cannot be taken back: check the address. The network fee is taken from the amount for \"all\" and " +
                            "added to it otherwise. The matcher waits for funds again when fewer than ${Kob.MIN_FUNDS_KAS.toInt()} KAS remain.",
                        12f,
                    )
                )
                sendButton = button("Withdraw ...") { review() }
                root.addView(sendButton)
                resultText = label("", 13f, selectable = true)
                root.addView(resultText)
                restartButton = button("Start the executor again") { restart() }.apply { visibility = View.GONE }
                root.addView(restartButton)
                root.addView(button("Back") { if (!busy) finish() })
            }
        )
        if (address.isEmpty() || !Kob.binary(this).isFile) sendButton.isEnabled = false
    }

    override fun onBackPressed() {
        if (!busy) super.onBackPressed() // a withdrawal in progress is not interrupted
    }

    private fun review() {
        val to = toEdit.text.toString().trim()
        val amount = amountEdit.text.toString().trim().removeSuffix("KAS").removeSuffix("kas").trim()
        when {
            to.isEmpty() -> return toast("Enter the address to send to")
            !to.startsWith("kaspatest:") -> return toast("A testnet-10 address starts with kaspatest:")
            amount.isEmpty() -> return toast("Enter an amount in KAS, or all")
            !amount.equals("all", ignoreCase = true) && !AMOUNT.matches(amount) ->
                return toast("The amount is a number of KAS (up to 8 decimals), or all")
        }
        val what = if (amount.equals("all", ignoreCase = true)) "everything on the key, minus the fee" else "$amount KAS"
        AlertDialog.Builder(this)
            .setTitle("Send $what?")
            .setMessage(
                "To:\n$to\n\nA transaction cannot be taken back. The executor is stopped first" +
                    (if (ExecutorService.state != "stopped") " (it is running now)" else "") + "."
            )
            .setPositiveButton("Send") { _, _ -> send(to, if (amount.equals("all", ignoreCase = true)) "all" else amount) }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun setBusy(on: Boolean) {
        busy = on
        for (v in listOf<View>(toEdit, amountEdit, allButton, sendButton, restartButton)) v.isEnabled = !on
    }

    private fun send(to: String, amount: String) {
        if (busy) return
        setBusy(true)
        restartButton.visibility = View.GONE
        wasRunning = ExecutorService.state != "stopped"
        resultText.text = if (wasRunning) "Stopping the executor ..." else "Working ..."
        work.execute {
            val text = try {
                stopExecutorAndWait()
                ui.post { resultText.text = "Sending ... (up to two minutes)" }
                val o = Kob.exec(
                    this, WITHDRAW_TIMEOUT_MS, "withdraw", "--to", to, "--amount", amount, "--yes", "--dir", Kob.dir(this).path
                )
                if (o.code == 0) "Done.\n\n" + o.out.trim() else "Nothing was sent.\n\n" + failure(o)
            } catch (e: Exception) {
                "Nothing was sent: ${e.message}"
            }
            ui.post {
                resultText.text = text
                setBusy(false)
                restartButton.visibility = View.VISIBLE
                restartButton.text = if (wasRunning) "Start the executor again" else "Start the executor"
            }
        }
    }

    /** The error lines of a failed run: the executor prints `Error: ...` (and the causes) on stderr, after its log lines. */
    private fun failure(o: Kob.Output): String {
        val lines = o.err.trim().lines()
        val at = lines.indexOfFirst { it.startsWith("Error:") }
        val shown = if (at >= 0) lines.drop(at) else lines.takeLast(6)
        return shown.joinToString("\n").take(1_200).ifBlank { "exit code ${o.code}" }
    }

    /** Stops the service and waits until no executor process holds the database (at most [STOP_WAIT_MS]); then kills a stray one. */
    private fun stopExecutorAndWait() {
        if (ExecutorService.state != "stopped" || Kob.running(this)) {
            startService(Intent(this, ExecutorService::class.java).setAction(ExecutorService.ACTION_STOP))
        }
        val until = SystemClock.elapsedRealtime() + STOP_WAIT_MS
        while (SystemClock.elapsedRealtime() < until && (ExecutorService.state != "stopped" || Kob.running(this))) Thread.sleep(250)
        if (Kob.running(this)) {
            Kob.killStale(this)
            Thread.sleep(1_000)
        }
        if (Kob.running(this)) throw RuntimeException("the executor did not stop (stop it from the main screen, then try again)")
    }

    private fun restart() {
        val i = Intent(this, ExecutorService::class.java)
        if (Build.VERSION.SDK_INT >= 26) startForegroundService(i) else startService(i)
        restartButton.visibility = View.GONE
        resultText.text = resultText.text.toString() + "\n\nThe executor is starting."
    }
}

package org.kob.executor

import android.app.Activity
import android.app.AlertDialog
import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.view.View
import android.view.WindowManager
import android.widget.Button
import android.widget.TextView
import java.io.File

/**
 * Shows the operator key so that it can be backed up or moved to another phone, behind an explicit warning and a confirmation.
 * The user can select and copy the text, or save it to a file they choose (the system file picker). The key is read from the
 * app's private file only when they confirm, kept in memory only while the screen is shown, and the screen cannot be captured
 * (FLAG_SECURE). Leaving the screen hides it again: showing it takes a new confirmation.
 */
class ExportKeyActivity : Activity() {
    private var key: String? = null
    private var picking = false // the file picker is on top: the screen is stopped but the key stays for the result
    private lateinit var warning: TextView
    private lateinit var showButton: Button
    private lateinit var keyText: TextView
    private lateinit var copyButton: Button
    private lateinit var saveButton: Button
    private lateinit var note: TextView

    private companion object {
        const val SAVE_REQUEST = 41
        val HEX64 = Regex("[0-9a-fA-F]{64}")
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        // no screenshots, no screen recording, no thumbnail in the recent apps while the key may be on screen
        window.setFlags(WindowManager.LayoutParams.FLAG_SECURE, WindowManager.LayoutParams.FLAG_SECURE)
        setContentView(
            page { root ->
                root.addView(label("Export the key", 22f, bold = true))
                warning = label(
                    "Anyone who sees this key can take every KAS on this address, and nobody can undo it. Do not photograph it, " +
                        "do not send it in a message, and do not store it where others can read it. Use it only to back it up " +
                        "(a password manager or an offline copy) or to move the executor to another phone.\n\n" +
                        "Uninstalling this app or clearing its data deletes the key. Without a copy of it, the funds on the " +
                        "address are lost.",
                    14f,
                    warning = true,
                )
                root.addView(warning)
                showButton = button("Show the key ...") { confirmShow() }
                root.addView(showButton)
                keyText = label("", 14f, mono = true, selectable = true).apply { visibility = View.GONE }
                root.addView(keyText)
                copyButton = button("Copy the key") { copyKey() }.apply { visibility = View.GONE }
                root.addView(copyButton)
                saveButton = button("Save the key to a file ...") { pickFile() }.apply { visibility = View.GONE }
                root.addView(saveButton)
                note = label("", 12f, selectable = true)
                root.addView(note)
                root.addView(
                    label(
                        "A copied key stays on the clipboard until something else replaces it, and other apps may read the " +
                            "clipboard: paste it where it belongs, then copy something else. A saved file is as secret as the key: " +
                            "choose a place you control (not a shared or synced folder).",
                        12f,
                    )
                )
                root.addView(button("Back") { finish() })
            }
        )
    }

    override fun onStop() {
        if (!picking) hideKey()
        super.onStop()
    }

    private fun confirmShow() {
        AlertDialog.Builder(this)
            .setTitle("Show the key?")
            .setMessage("It lets anyone who sees it take the funds on this address. Make sure nobody is looking at the screen.")
            .setPositiveButton("Show the key") { _, _ -> showKey() }
            .setNegativeButton("Cancel", null)
            .show()
    }

    private fun showKey() {
        val text = try {
            File(Kob.dir(this), Kob.KEY_FILE).readText().trim()
        } catch (e: Exception) {
            note.text = "The key file cannot be read: ${e.message}. Set the app up first."
            return
        }
        if (!HEX64.matches(text)) {
            note.text = "The key file is not a 64-character hex key."
            return
        }
        key = text
        keyText.text = text
        keyText.visibility = View.VISIBLE
        copyButton.visibility = View.VISIBLE
        saveButton.visibility = View.VISIBLE
        showButton.visibility = View.GONE
        note.text = "Long-press the key to select part of it, or use the buttons."
    }

    private fun hideKey() {
        key = null
        keyText.text = ""
        keyText.visibility = View.GONE
        copyButton.visibility = View.GONE
        saveButton.visibility = View.GONE
        showButton.visibility = View.VISIBLE
        note.text = ""
    }

    private fun copyKey() {
        val k = key ?: return
        copyText("KOB operator key", k, sensitive = true)
        toast("Key copied: paste it where it belongs, then copy something else")
    }

    @Suppress("DEPRECATION") // no AndroidX in this app: the plain Activity result API
    private fun pickFile() {
        val i = Intent(Intent.ACTION_CREATE_DOCUMENT).apply {
            addCategory(Intent.CATEGORY_OPENABLE)
            type = "text/plain"
            putExtra(Intent.EXTRA_TITLE, "kob-operator.key")
        }
        try {
            picking = true
            startActivityForResult(i, SAVE_REQUEST)
        } catch (e: Exception) {
            picking = false
            note.text = "This phone has no file picker to save to: ${e.message}"
        }
    }

    @Suppress("DEPRECATION")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        picking = false
        if (requestCode != SAVE_REQUEST || resultCode != RESULT_OK) return
        val uri: Uri = data?.data ?: return
        val k = key ?: run {
            note.text = "The key was hidden before the file was chosen: show it again."
            return
        }
        note.text = try {
            contentResolver.openOutputStream(uri, "wt")!!.use { it.write(k.toByteArray(Charsets.US_ASCII)) }
            "Saved. The file holds the key as plain text: keep it as secret as the key."
        } catch (e: Exception) {
            "The file could not be written: ${e.message}"
        }
    }
}

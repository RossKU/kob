package org.kob.executor

import android.app.Activity
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.graphics.Typeface
import android.os.Build
import android.os.PersistableBundle
import android.util.TypedValue
import android.view.View
import android.view.WindowInsets
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast

// Plain framework views shared by the screens (no layout files, no library): text colours come from the theme, which follows the
// system dark setting (values-night), so every screen reads the same in light and dark.

fun Context.dp(v: Int) = (v * resources.displayMetrics.density).toInt()

fun Context.label(
    s: String,
    sp: Float = 14f,
    bold: Boolean = false,
    mono: Boolean = false,
    selectable: Boolean = false,
    warning: Boolean = false,
) = TextView(this).apply {
    text = s
    if (selectable) setTextIsSelectable(true) // long-press selects, then copy
    setTextSize(TypedValue.COMPLEX_UNIT_SP, sp)
    if (bold) setTypeface(typeface, Typeface.BOLD)
    if (mono) typeface = Typeface.MONOSPACE
    if (warning) setTextColor(getColor(R.color.kob_warning))
    setPadding(0, dp(4), 0, dp(4))
}

fun Context.button(s: String, onClick: () -> Unit) = Button(this).apply {
    text = s
    isAllCaps = false
    setOnClickListener { onClick() }
}

fun Context.toast(s: String) = Toast.makeText(this, s, Toast.LENGTH_SHORT).show()

fun Context.textField(hint: String, mono: Boolean = false, inputType: Int = android.text.InputType.TYPE_CLASS_TEXT) = EditText(this).apply {
    this.hint = hint
    this.inputType = inputType
    if (mono) typeface = Typeface.MONOSPACE
    setSingleLine()
}

/** A vertical page that scrolls and, edge-to-edge (Android 15+), keeps its content clear of the system bars. */
fun Activity.page(build: (LinearLayout) -> Unit): View {
    val root = LinearLayout(this).apply {
        orientation = LinearLayout.VERTICAL
        setPadding(dp(16), dp(16), dp(16), dp(24))
    }
    build(root)
    return ScrollView(this).apply {
        addView(root)
        setOnApplyWindowInsetsListener { v, insets ->
            if (Build.VERSION.SDK_INT >= 30) {
                val b = insets.getInsets(WindowInsets.Type.systemBars())
                v.setPadding(b.left, b.top, b.right, b.bottom)
            }
            insets
        }
    }
}

/** Copies [text] to the clipboard; [sensitive] asks Android (13+) not to show the copied text in its clipboard preview. */
fun Context.copyText(what: String, text: String, sensitive: Boolean = false) {
    val clip = ClipData.newPlainText(what, text)
    if (sensitive && Build.VERSION.SDK_INT >= 33) {
        clip.description.extras = PersistableBundle().apply { putBoolean("android.content.extra.IS_SENSITIVE", true) }
    }
    getSystemService(ClipboardManager::class.java).setPrimaryClip(clip)
}

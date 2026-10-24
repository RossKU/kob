package org.kob.executor

import android.Manifest
import android.app.Activity
import android.content.ActivityNotFoundException
import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.content.Intent
import android.content.SharedPreferences
import android.content.pm.PackageManager
import android.graphics.Typeface
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.provider.Settings
import android.text.InputType
import android.util.TypedValue
import android.view.View
import android.view.WindowInsets
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.RadioButton
import android.widget.RadioGroup
import android.widget.ScrollView
import android.widget.Switch
import android.widget.TextView
import android.widget.Toast
import org.json.JSONObject
import java.io.File
import java.net.HttpURLConnection
import java.net.URL
import java.util.concurrent.Executors

/** Three sections on one screen: setup (node, role, key, funding address), the start / stop switch, and the status. */
class MainActivity : Activity() {
    private val ui = Handler(Looper.getMainLooper())
    private val work = Executors.newSingleThreadExecutor() // setup (the executor binary)
    private val funds = Executors.newSingleThreadExecutor() // the balance: it may wait for the node for a minute, setup must not queue behind it
    private val net = Executors.newSingleThreadExecutor() // the read API
    private lateinit var prefs: SharedPreferences

    private lateinit var publicRadio: RadioButton
    private lateinit var ownRadio: RadioButton
    private lateinit var nodeGroup: RadioGroup
    private lateinit var urlEdit: EditText
    private lateinit var matchRadio: RadioButton
    private lateinit var indexRadio: RadioButton
    private lateinit var roleGroup: RadioGroup
    private lateinit var saveButton: Button
    private lateinit var setupNote: TextView
    private lateinit var fundingBox: LinearLayout
    private lateinit var addressText: TextView
    private lateinit var balanceText: TextView
    private lateinit var runSwitch: Switch
    private lateinit var statusText: TextView
    private lateinit var logText: TextView
    private lateinit var logScroll: ScrollView

    @Volatile private var health: JSONObject? = null
    @Volatile private var healthError: String? = null
    @Volatile private var healthBusy = false
    @Volatile private var balance: String = "not read yet"
    @Volatile private var balanceKas: Double? = null
    @Volatile private var balanceAtMs = 0L
    @Volatile private var balanceOkAtMs = 0L // the last successful read
    @Volatile private var balanceOk = false
    @Volatile private var balanceLast: String? = null // the last good value as text
    @Volatile private var balanceBusy = false
    private var updatingSwitch = false

    private val tick = object : Runnable {
        override fun run() {
            render()
            ui.postDelayed(this, 2_000)
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        prefs = getSharedPreferences(ExecutorService.PREFS, Context.MODE_PRIVATE)
        setContentView(buildUi())
        loadSettings()
    }

    override fun onResume() {
        super.onResume()
        balanceAtMs = 0 // read the balance now
        ui.post(tick)
    }

    override fun onPause() {
        ui.removeCallbacks(tick)
        super.onPause()
    }

    // ---- UI ----

    private fun dp(v: Int) = (v * resources.displayMetrics.density).toInt()

    private fun label(s: String, sp: Float = 14f, bold: Boolean = false, mono: Boolean = false, selectable: Boolean = false) = TextView(this).apply {
        text = s
        if (selectable) setTextIsSelectable(true) // long-press selects, then copy
        setTextSize(TypedValue.COMPLEX_UNIT_SP, sp)
        if (bold) setTypeface(typeface, Typeface.BOLD)
        if (mono) typeface = Typeface.MONOSPACE
        setPadding(0, dp(4), 0, dp(4))
    }

    private fun button(s: String, onClick: () -> Unit) = Button(this).apply {
        text = s
        isAllCaps = false
        setOnClickListener { onClick() }
    }

    private fun radio(s: String) = RadioButton(this).apply {
        text = s
        id = View.generateViewId()
    }

    private fun buildUi(): View {
        val root = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(dp(16), dp(16), dp(16), dp(24))
        }
        fun add(v: View) = root.addView(v)

        add(label("KOB executor (testnet-10)", 22f, bold = true))
        add(label("No telemetry, no fee, no project server: it talks only to the Kaspa node(s) you choose.", 12f))

        // (a) setup
        add(label("1. Setup", 18f, bold = true))
        publicRadio = radio("Public nodes (two, found through the Kaspa resolvers)")
        ownRadio = radio("My own node")
        nodeGroup = RadioGroup(this).apply {
            orientation = RadioGroup.VERTICAL
            addView(publicRadio)
            addView(ownRadio)
            setOnCheckedChangeListener { _, _ -> urlEdit.visibility = if (ownRadio.isChecked) View.VISIBLE else View.GONE }
        }
        add(nodeGroup)
        urlEdit = EditText(this).apply {
            hint = "ws://host:18210 (JSON wRPC; needs the UTXO index for the matcher)"
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_VARIATION_URI
            setSingleLine()
        }
        add(urlEdit)
        matchRadio = radio("Matcher and keepers (needs a funded hot key)")
        indexRadio = radio("Index only (no key, no funds)")
        roleGroup = RadioGroup(this).apply {
            orientation = RadioGroup.VERTICAL
            addView(matchRadio)
            addView(indexRadio)
        }
        add(roleGroup)
        saveButton = button("Save and generate key") { saveSettings() }
        add(saveButton)
        setupNote = label("", 12f)
        add(setupNote)

        fundingBox = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        fundingBox.addView(label("Funding address (send testnet KAS here; at least 100, 200 recommended, small funds only)", 13f))
        addressText = label("", 13f, mono = true, selectable = true)
        fundingBox.addView(addressText)
        fundingBox.addView(button("Copy address") { copyAddress() })
        balanceText = label("", 14f, bold = true, selectable = true)
        fundingBox.addView(balanceText)
        fundingBox.addView(
            label(
                "The key is stored only in this app's private storage. Uninstalling the app or clearing its data deletes it, " +
                    "and the funds with it.",
                12f
            )
        )
        add(fundingBox)

        // (b) start / stop
        add(label("2. Run", 18f, bold = true))
        runSwitch = Switch(this).apply {
            text = "Run the executor"
            setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
            setOnCheckedChangeListener { _, on -> if (!updatingSwitch) if (on) startExecutor() else stopExecutor() }
        }
        add(runSwitch)
        add(button("Battery: allow unrestricted background use") { batterySettings() })
        add(
            label(
                "First start downloads about 4 GB: use Wi-Fi and a charger. Android may still stop apps in the background: " +
                    "turn battery optimisation off for this app.",
                12f
            )
        )

        // (c) status
        add(label("3. Status", 18f, bold = true))
        statusText = label("", 14f, selectable = true)
        add(statusText)
        add(label("Log (scrolls; long-press to select and copy)", 13f, bold = true))
        logText = label("", 10f, mono = true, selectable = true)
        logScroll = ScrollView(this).apply {
            addView(logText)
            // the log scrolls inside the page: its own touches are not taken over by the page's scrolling
            setOnTouchListener { v, _ ->
                v.parent?.requestDisallowInterceptTouchEvent(true)
                false
            }
        }
        root.addView(logScroll, LinearLayout.LayoutParams(LinearLayout.LayoutParams.MATCH_PARENT, dp(260)))

        return ScrollView(this).apply {
            addView(root)
            // edge-to-edge (Android 15+): keep the content clear of the system bars
            setOnApplyWindowInsetsListener { v, insets ->
                if (Build.VERSION.SDK_INT >= 30) {
                    val b = insets.getInsets(WindowInsets.Type.systemBars())
                    v.setPadding(b.left, b.top, b.right, b.bottom)
                }
                insets
            }
        }
    }

    // ---- settings and setup ----

    private fun configured() = File(Kob.dir(this), "kob-executor.toml").isFile

    private fun role() = prefs.getString("role", "match") ?: "match"

    private fun loadSettings() {
        val own = prefs.getString("node", "resolver") == "own"
        nodeGroup.check(if (own) ownRadio.id else publicRadio.id)
        urlEdit.setText(prefs.getString("url", ""))
        urlEdit.visibility = if (own) View.VISIBLE else View.GONE
        roleGroup.check(if (role() == "index") indexRadio.id else matchRadio.id)
        if (!Kob.binary(this).isFile) setupNote.text = "This build has no executor binary (${Kob.binary(this).path})."
    }

    private fun saveSettings() {
        val own = ownRadio.isChecked
        val url = urlEdit.text.toString().trim()
        if (own && url.isEmpty()) {
            toast("Enter the URL of your node")
            return
        }
        val role = if (indexRadio.isChecked) "index" else "match"
        saveButton.isEnabled = false
        setupNote.text = "Working ..."
        work.execute {
            val msg = try {
                val out = Kob.run(
                    this, 60_000, "setup", "--non-interactive", "--node", if (own) url else "resolver", "--role", role,
                    "--dir", Kob.dir(this).path, "--new-key"
                )
                val j = JSONObject(out.trim().lines().last { it.startsWith("{") })
                prefs.edit()
                    .putString("node", if (own) "own" else "resolver")
                    .putString("url", url)
                    .putString("role", role)
                    .putString("address", if (j.isNull("address")) "" else j.getString("address"))
                    .apply()
                balanceAtMs = 0
                "Saved: ${if (role == "index") "index only" else "matcher and keepers"}, ${j.optString("node")}."
            } catch (e: Exception) {
                "Setup failed: ${e.message}"
            }
            ui.post {
                setupNote.text = msg
                render()
            }
        }
    }

    private fun copyAddress() {
        val a = prefs.getString("address", "") ?: ""
        if (a.isEmpty()) return
        getSystemService(ClipboardManager::class.java).setPrimaryClip(ClipData.newPlainText("KOB funding address", a))
        toast("Address copied")
    }

    private fun batterySettings() {
        val pm = getSystemService(PowerManager::class.java)
        if (pm.isIgnoringBatteryOptimizations(packageName)) {
            toast("Already unrestricted")
            return
        }
        try {
            startActivity(Intent(Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS, Uri.parse("package:$packageName")))
        } catch (e: ActivityNotFoundException) {
            startActivity(Intent(Settings.ACTION_IGNORE_BATTERY_OPTIMIZATION_SETTINGS))
        }
    }

    private fun toast(s: String) = Toast.makeText(this, s, Toast.LENGTH_SHORT).show()

    // ---- start / stop ----

    private fun setSwitch(on: Boolean) {
        if (runSwitch.isChecked == on) return
        updatingSwitch = true
        runSwitch.isChecked = on
        updatingSwitch = false
    }

    private fun startExecutor() {
        if (!configured() || !Kob.binary(this).isFile) {
            toast("Save the setup first")
            setSwitch(false)
            return
        }
        if (Build.VERSION.SDK_INT >= 33 && checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.POST_NOTIFICATIONS), 1)
        }
        val i = Intent(this, ExecutorService::class.java)
        if (Build.VERSION.SDK_INT >= 26) startForegroundService(i) else startService(i)
        health = null
        healthError = null
    }

    private fun stopExecutor() {
        startService(Intent(this, ExecutorService::class.java).setAction(ExecutorService.ACTION_STOP))
    }

    // ---- status ----

    private fun render() {
        val st = ExecutorService.state
        val running = st != "stopped"
        setSwitch(running)
        val ok = configured()
        val index = role() == "index"
        // the settings do not change under a running executor
        for (v in listOf<View>(publicRadio, ownRadio, urlEdit, matchRadio, indexRadio, saveButton)) v.isEnabled = !running
        if (running && !healthBusy) fetchHealth()
        val address = prefs.getString("address", "") ?: ""
        fundingBox.visibility = if (ok && !index && address.isNotEmpty()) View.VISIBLE else View.GONE
        setLive(addressText, address)
        if (fundingBox.visibility == View.VISIBLE) {
            setLive(balanceText, "Balance: $balance")
            // read again every minute, and sooner (every 15 s) while the last read failed
            val every = if (balanceOk) 60_000L else 15_000L
            if (!balanceBusy && System.currentTimeMillis() - balanceAtMs > every) fetchBalance()
        }
        runSwitch.isEnabled = ok

        val s = StringBuilder()
        val warnings = mutableListOf<String>()
        if (!ok) s.append("Not set up yet.\n")
        s.append("Executor: ")
        when (st) {
            "running" -> s.append("running for ${human((System.currentTimeMillis() - ExecutorService.startedAtMs) / 1000)}")
            "restarting" -> {
                s.append("restarting in ${maxOf(0, (ExecutorService.retryAtMs - System.currentTimeMillis()) / 1000)} s")
                warnings += "the executor ended (${ExecutorService.lastExit}) and is started again"
            }
            else -> s.append("stopped")
        }
        if (ExecutorService.restarts > 0) s.append(" (restarted ${ExecutorService.restarts}x)")
        s.append('\n')

        val h = health
        if (running && h != null) {
            val lag = if (h.isNull("lag_seconds")) null else h.optLong("lag_seconds")
            val caught = h.optBoolean("caught_up")
            s.append("Indexer: ${h.optString("state")}, ")
            s.append(if (caught) "caught up" else if (lag != null) "${human(lag)} behind the chain" else "node position unknown")
            if (lag != null && caught) s.append(" (${human(lag)} behind)")
            s.append(", ${h.optLong("orders_total")} orders indexed\n")
            val nodes = h.optJSONArray("nodes")
            if (nodes != null) {
                for (i in 0 until nodes.length()) {
                    val n = nodes.getJSONObject(i)
                    val host = n.optString("node").ifEmpty { n.optString("url") }
                    val role = if (n.optString("role") == "primary") "chain authority" else "cross-check"
                    s.append("Node: $host ($role, ${n.optString("state")})\n")
                }
            }
            if (!caught && lag != null && lag > 3600) warnings += "catching up: ${human(lag)} behind (the first catch-up takes hours)"
            if (!h.isNull("last_error") && h.optString("state") != "following") warnings += "node: ${h.optString("last_error")}"
        } else if (running) {
            s.append("Read API: ${healthError ?: "waiting for it"}\n")
        }
        val line = statusLine()
        if (running && line != null) {
            // "indexer ... | nodes ... | N transactions sent, ... | funds X KAS | no warnings"
            for (part in line.split(" | ")) if (!part.startsWith("indexer ") && !part.startsWith("nodes: ")) s.append(part).append('\n')
        }
        val kas = balanceKas
        if (ok && !index && kas != null && kas < Kob.MIN_FUNDS_KAS) warnings += "funds below ${Kob.MIN_FUNDS_KAS.toInt()} KAS: the matcher waits for more"
        s.append(if (warnings.isEmpty()) "Warnings: none" else "Warnings:\n" + warnings.joinToString("\n") { " - $it" })
        setLive(statusText, s.toString())
        val log = ExecutorService.lastLines(200).joinToString("\n") { it.take(300) }
        if (!logText.hasSelection() && logText.text.toString() != log) {
            // keep following the end of the log unless the user scrolled up to read it
            val atEnd = logScroll.getChildAt(0).bottom - (logScroll.height + logScroll.scrollY) < dp(40)
            logText.text = log
            if (atEnd) logScroll.post { logScroll.fullScroll(View.FOCUS_DOWN) }
        }
    }

    /** Sets a selectable text only when it changed and while nothing in it is selected: the periodic refresh must not wipe a selection. */
    private fun setLive(tv: TextView, text: String) {
        if (tv.hasSelection() || tv.text.toString() == text) return
        tv.text = text
    }

    /** The executor's last console status line (logged every minute). */
    private fun statusLine(): String? {
        val l = ExecutorService.lastLines(300).lastOrNull { it.contains(" status: ") } ?: return null
        return l.substringAfter(" status: ")
    }

    private fun human(secs: Long): String = when {
        secs < 120 -> "$secs s"
        secs < 7_200 -> "${secs / 60} min"
        secs < 172_800 -> "${secs / 3600} h ${secs % 3600 / 60} min"
        else -> "${secs / 86_400} d"
    }

    private fun fetchHealth() {
        healthBusy = true
        net.execute {
            var c: HttpURLConnection? = null
            try {
                c = URL(Kob.API + "/v1/health").openConnection() as HttpURLConnection
                c.connectTimeout = 1_500
                c.readTimeout = 3_000
                val body = c.inputStream.bufferedReader().readText()
                health = JSONObject(body)
                healthError = null
            } catch (e: Exception) {
                health = null
                healthError = "not reachable yet (${e.javaClass.simpleName})"
            } finally {
                c?.disconnect()
                healthBusy = false
            }
        }
    }

    /**
     * The funds of the operator address. A running executor logs them every minute (from its own node), so that is used first;
     * otherwise `address-balance` asks ONE public node (or the configured one) in a background thread, with 60 s allowed for
     * the connection over a mobile network. A failed read keeps the last good value on screen and is tried again soon.
     */
    private fun fetchBalance() {
        balanceBusy = true
        balanceAtMs = System.currentTimeMillis()
        val logged = if (ExecutorService.state == "running") statusLine()?.let { FUNDS.find(it)?.groupValues?.get(1) } else null
        if (logged != null) {
            balanceKas = logged.toDouble()
            balanceLast = "$logged KAS"
            balance = "$logged KAS (from the running executor)"
            balanceOk = true
            balanceOkAtMs = balanceAtMs
            balanceBusy = false
            return
        }
        funds.execute {
            try {
                val out = Kob.run(this, 60_000, "address-balance", "--dir", Kob.dir(this).path)
                val j = JSONObject(out.trim().lines().last { it.startsWith("{") })
                if (j.isNull("balance_sompi")) {
                    failBalance(j.optString("error"))
                } else {
                    balanceKas = j.getString("balance_kas").toDouble()
                    balanceLast = "${j.getString("balance_kas")} KAS"
                    balance = balanceLast!!
                    balanceOk = true
                    balanceOkAtMs = System.currentTimeMillis()
                }
            } catch (e: Exception) {
                failBalance(e.message ?: e.javaClass.simpleName)
            } finally {
                balanceBusy = false
            }
        }
    }

    private fun failBalance(why: String) {
        balanceOk = false
        val last = balanceLast
        balance = if (last != null) {
            "$last (read ${human((System.currentTimeMillis() - balanceOkAtMs) / 1000)} ago; now unavailable: $why; trying again)"
        } else {
            "unavailable ($why); trying again"
        }
    }

    private companion object {
        val FUNDS = Regex("""funds ([0-9]+(?:\.[0-9]+)?) KAS""")
    }
}

package com.pydmg.neogeo

import android.content.Intent
import android.net.Uri
import android.os.Bundle
import android.text.Editable
import android.text.TextWatcher
import android.util.Log
import android.view.View
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import android.widget.Toast
import androidx.activity.result.contract.ActivityResultContracts
import androidx.appcompat.app.AppCompatActivity
import androidx.documentfile.provider.DocumentFile
import androidx.recyclerview.widget.LinearLayoutManager
import androidx.recyclerview.widget.RecyclerView
import com.google.android.material.materialswitch.MaterialSwitch
import com.google.android.material.slider.Slider
import com.google.android.material.tabs.TabLayout
import java.util.ArrayDeque

/**
 * Frontend / launcher activity (portrait).
 *
 * Two tabs:
 *   1. "Biblioteca" — ROM list + search + load-zip / pick-folder buttons.
 *   2. "Ajustes"   — control, video and storage preferences.
 *
 * Tapping a ROM card:
 *   1. (Lazily) loads the BIOS bytes into the native core.
 *   2. Loads the cart bytes.
 *   3. Launches EmulatorActivity (landscape, fullscreen).
 *
 * No bottom-drawer / lateral drawer here — that was the source of the
 * v2 crash (M2/M3 widget mix). Everything is a flat portrait layout.
 */
class LibraryActivity : AppCompatActivity() {

    private lateinit var tabs: TabLayout
    private lateinit var libraryView: View
    private lateinit var settingsView: ScrollView
    private lateinit var libraryList: RecyclerView
    private lateinit var libraryEmptyView: View
    private lateinit var libraryCount: TextView
    private lateinit var editSearch: EditText
    private lateinit var biosChip: Button
    private lateinit var textFolderValue: TextView

    private lateinit var swJoystick: MaterialSwitch
    private lateinit var swHaptics: MaterialSwitch
    private lateinit var swLocalMp: MaterialSwitch
    private lateinit var swCrop: MaterialSwitch
    private lateinit var swSmooth: MaterialSwitch
    private lateinit var sliderOpacity: Slider
    private lateinit var sliderScale: Slider

    private val adapter: RomLibraryAdapter by lazy {
        RomLibraryAdapter(emptyList()) { entry ->
            onRomClicked(entry)
        }
    }

    // ---------- SAF launchers ----------
    private val pickBios = registerForActivityResult(
        ActivityResultContracts.OpenDocument()
    ) { uri ->
        if (uri == null) return@registerForActivityResult
        persistReadPermission(uri)
        PydmgApp.prefs.biosUri = uri
        PydmgApp.prefs.biosLabel = displayName(uri) ?: "neogeo.zip"
        loadBiosIntoCache(uri, showToast = true)
    }

    private val pickCart = registerForActivityResult(
        ActivityResultContracts.OpenDocument()
    ) { uri ->
        if (uri == null) return@registerForActivityResult
        persistReadPermission(uri)
        val name = (displayName(uri) ?: "cart.zip").substringBeforeLast('.').lowercase()
        Thread {
            val bytes = readAllBytes(uri)
            if (bytes == null) { toast(getString(R.string.err_cart)); return@Thread }
            launchCart(RomEntry(name, uri, isBios = false, sizeBytes = bytes.size.toLong()), preloaded = bytes)
        }.start()
    }

    private val pickFolder = registerForActivityResult(
        ActivityResultContracts.OpenDocumentTree()
    ) { uri ->
        if (uri == null) return@registerForActivityResult
        persistReadPermission(uri)
        PydmgApp.prefs.romFolderUri = uri
        updateFolderText()
        scanFolder(showToast = true)
    }

    // ---------- Lifecycle ----------

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        setContentView(R.layout.activity_library)

        NativeBridge.nativeInitLogger()
        // Idempotent: re-creating the System after we've come back from the
        // emulator is safe and clears stale state.
        NativeBridge.nativeCreate(NativeBridge.HW_MVS)

        bindViews()
        wireTabs()
        wireLibrary()
        wireSettings()

        updateBiosChip()
        updateFolderText()

        // Restore previously-loaded BIOS bytes if we have any cached.
        PydmgApp.prefs.biosUri?.let { biosUri ->
            if (PydmgApp.app.biosBytesCache == null) {
                Thread {
                    val bytes = readAllBytes(biosUri)
                    if (bytes != null) {
                        PydmgApp.app.biosBytesCache = bytes
                        Log.i(TAG, "BIOS cached on init (${bytes.size} bytes)")
                    }
                }.start()
            }
        }

        // First scan of the configured ROM folder, if any.
        if (PydmgApp.prefs.romFolderUri != null) {
            scanFolder(showToast = false)
        } else {
            adapter.submit(emptyList())
            updateEmptyState()
        }
    }

    private fun bindViews() {
        tabs = findViewById(R.id.tabs)
        libraryView = findViewById(R.id.library_view)
        settingsView = findViewById(R.id.settings_view)
        libraryList = findViewById(R.id.library_list)
        libraryEmptyView = findViewById(R.id.library_empty_view)
        libraryCount = findViewById(R.id.library_count)
        editSearch = findViewById(R.id.edit_search)
        biosChip = findViewById(R.id.btn_bios_chip)
        textFolderValue = findViewById(R.id.text_folder_value)

        swJoystick = findViewById(R.id.sw_use_joystick)
        swLocalMp = findViewById(R.id.sw_local_mp)
        swHaptics = findViewById(R.id.sw_haptics)
        swCrop = findViewById(R.id.sw_crop)
        swSmooth = findViewById(R.id.sw_smooth)
        sliderOpacity = findViewById(R.id.slider_opacity)
        sliderScale = findViewById(R.id.slider_scale)
    }

    private fun wireTabs() {
        tabs.addTab(tabs.newTab().setText(R.string.tab_library))
        tabs.addTab(tabs.newTab().setText(R.string.tab_settings))
        tabs.addOnTabSelectedListener(object : TabLayout.OnTabSelectedListener {
            override fun onTabSelected(tab: TabLayout.Tab) {
                val showLibrary = tab.position == 0
                libraryView.visibility = if (showLibrary) View.VISIBLE else View.GONE
                settingsView.visibility = if (showLibrary) View.GONE else View.VISIBLE
            }
            override fun onTabUnselected(tab: TabLayout.Tab) {}
            override fun onTabReselected(tab: TabLayout.Tab) {}
        })
    }

    private fun wireLibrary() {
        libraryList.layoutManager = LinearLayoutManager(this)
        libraryList.adapter = adapter

        findViewById<Button>(R.id.btn_load_zip).setOnClickListener {
            pickCart.launch(arrayOf("*/*"))
        }
        findViewById<Button>(R.id.btn_pick_folder).setOnClickListener {
            pickFolder.launch(null)
        }
        findViewById<Button>(R.id.btn_rescan_top).setOnClickListener {
            scanFolder(showToast = true)
        }

        biosChip.setOnClickListener {
            pickBios.launch(arrayOf("*/*"))
        }

        editSearch.addTextChangedListener(object : TextWatcher {
            override fun afterTextChanged(s: Editable?) {
                applyFilter(s?.toString().orEmpty())
            }
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) {}
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) {}
        })
    }

    private fun wireSettings() {
        // Initial values
        swJoystick.isChecked = PydmgApp.prefs.useJoystick
        swLocalMp.isChecked = PydmgApp.prefs.localMultiplayer
        swHaptics.isChecked = PydmgApp.prefs.hapticFeedback
        swCrop.isChecked = PydmgApp.prefs.cropScreen
        swSmooth.isChecked = PydmgApp.prefs.smoothFilter
        sliderOpacity.value = PydmgApp.prefs.controlOpacity.coerceIn(0.25f, 1.0f)
        sliderScale.value = PydmgApp.prefs.controlScale.coerceIn(0.75f, 1.35f)

        swJoystick.setOnCheckedChangeListener { _, c -> PydmgApp.prefs.useJoystick = c }
        swLocalMp.setOnCheckedChangeListener { _, c -> PydmgApp.prefs.localMultiplayer = c }
        swHaptics.setOnCheckedChangeListener { _, c -> PydmgApp.prefs.hapticFeedback = c }
        swCrop.setOnCheckedChangeListener { _, c -> PydmgApp.prefs.cropScreen = c }
        swSmooth.setOnCheckedChangeListener { _, c -> PydmgApp.prefs.smoothFilter = c }
        sliderOpacity.addOnChangeListener { _, v, _ -> PydmgApp.prefs.controlOpacity = v }
        sliderScale.addOnChangeListener { _, v, _ -> PydmgApp.prefs.controlScale = v }

        findViewById<Button>(R.id.btn_settings_pick_folder).setOnClickListener {
            pickFolder.launch(null)
        }
        findViewById<Button>(R.id.btn_settings_rescan).setOnClickListener {
            scanFolder(showToast = true)
        }
        findViewById<Button>(R.id.btn_settings_pick_bios).setOnClickListener {
            pickBios.launch(arrayOf("*/*"))
        }
    }

    // ---------- Library mechanics ----------

    private fun scanFolder(showToast: Boolean) {
        val folder = PydmgApp.prefs.romFolderUri
        if (folder == null) {
            if (showToast) toast(getString(R.string.err_folder))
            return
        }
        Thread {
            try {
                val root = DocumentFile.fromTreeUri(this, folder)
                if (root == null || !root.isDirectory) {
                    toast(getString(R.string.err_folder)); return@Thread
                }
                val found = ArrayList<RomEntry>()
                val q = ArrayDeque<DocumentFile>()
                q.add(root)
                var guard = 0
                while (q.isNotEmpty() && guard < 5000) {
                    val node = q.removeFirst(); guard++
                    if (node.isDirectory) {
                        node.listFiles().forEach { q.add(it) }
                    } else if (node.isFile) {
                        val name = (node.name ?: "").lowercase()
                        if (name.endsWith(".zip")) {
                            found += RomEntry(
                                name = name.substringBeforeLast('.'),
                                uri = node.uri,
                                isBios = (name == "neogeo.zip"),
                                sizeBytes = node.length(),
                            )
                        }
                    }
                }
                found.sortWith(compareBy<RomEntry> { !it.isBios }.thenBy { it.name })
                PydmgApp.app.library = found
                runOnUiThread {
                    applyFilter(editSearch.text?.toString().orEmpty())
                    if (showToast) toast("ROMs detectadas: ${found.size}")
                }
            } catch (t: Throwable) {
                Log.e(TAG, "scanFolder", t)
                if (showToast) toast(getString(R.string.err_folder))
            }
        }.start()
    }

    private fun applyFilter(query: String) {
        val all = PydmgApp.app.library
        val filtered = if (query.isBlank()) all
            else all.filter { it.name.contains(query.lowercase()) }
        adapter.submit(filtered)
        libraryCount.text = getString(R.string.library_subtitle_count, filtered.size)
        updateEmptyState(filtered)
    }

    private fun updateEmptyState(visible: List<RomEntry> = adapter.let { _ -> currentItems() }) {
        val empty = visible.isEmpty()
        libraryEmptyView.visibility = if (empty) View.VISIBLE else View.GONE
        libraryList.visibility = if (empty) View.GONE else View.VISIBLE
    }

    private fun currentItems(): List<RomEntry> = PydmgApp.app.library

    // ---------- BIOS / cart loading ----------

    private fun loadBiosIntoCache(uri: Uri, showToast: Boolean) {
        Thread {
            val bytes = readAllBytes(uri)
            if (bytes == null) {
                if (showToast) toast(getString(R.string.err_bios))
                return@Thread
            }
            val ok = NativeBridge.nativeLoadBiosZip(bytes)
            if (ok) {
                PydmgApp.app.biosBytesCache = bytes
                runOnUiThread { updateBiosChip() }
                if (showToast) toast(getString(R.string.ready))
            } else if (showToast) {
                toast(getString(R.string.err_bios))
            }
            Log.i(TAG, "BIOS load ${if (ok) "OK" else "FAIL"} (${bytes.size} bytes)")
        }.start()
    }

    private fun onRomClicked(entry: RomEntry) {
        if (entry.isBios) {
            // Treat as BIOS pick.
            PydmgApp.prefs.biosUri = entry.uri
            PydmgApp.prefs.biosLabel = entry.name
            loadBiosIntoCache(entry.uri, showToast = true)
            return
        }
        // Cart launch.
        Thread {
            val bytes = readAllBytes(entry.uri)
            if (bytes == null) {
                toast(getString(R.string.err_cart)); return@Thread
            }
            launchCart(entry, preloaded = bytes)
        }.start()
    }

    /**
     * Stages BIOS bytes (if any) and the cart bytes into the Rust core,
     * then launches EmulatorActivity. Runs on a worker thread.
     */
    private fun launchCart(entry: RomEntry, preloaded: ByteArray) {
        toast(getString(R.string.loading_cart, entry.name))
        ensureBiosStaged()

        val ok = NativeBridge.nativeLoadCartZip(entry.name, preloaded)
        Log.i(TAG, "Cart '${entry.name}' load=$ok (${preloaded.size} bytes)")
        if (!ok) { toast(getString(R.string.err_cart)); return }

        PydmgApp.prefs.lastCartName = entry.name
        runOnUiThread {
            // If LAN netplay is enabled, detour through the discovery /
            // handshake wizard first. It'll launch EmulatorActivity
            // itself once the socket is up. Otherwise go direct.
            val target = if (PydmgApp.prefs.lanMultiplayer)
                NetplayActivity::class.java
            else
                EmulatorActivity::class.java
            startActivity(Intent(this, target))
        }
    }

    /**
     * Re-stage BIOS bytes if we have any cached. The Rust side consumes
     * the staged RomSet inside `nativeLoadCartZip`, so we must restage
     * before each cart launch.
     */
    private fun ensureBiosStaged() {
        PydmgApp.app.biosBytesCache?.let { bytes ->
            NativeBridge.nativeLoadBiosZip(bytes)
            return
        }
        // No manual BIOS yet — try auto-pick from the library.
        PydmgApp.app.library.firstOrNull { it.isBios }?.let { biosEntry ->
            val bytes = readAllBytes(biosEntry.uri) ?: return@let
            if (NativeBridge.nativeLoadBiosZip(bytes)) {
                PydmgApp.app.biosBytesCache = bytes
                PydmgApp.prefs.biosUri = biosEntry.uri
                PydmgApp.prefs.biosLabel = biosEntry.name
                runOnUiThread { updateBiosChip() }
            }
        }
    }

    // ---------- UI helpers ----------

    private fun updateBiosChip() {
        val label = PydmgApp.prefs.biosLabel
        biosChip.text = if (label.isBlank()) getString(R.string.bios_missing)
            else getString(R.string.bios_loaded, label)
    }

    private fun updateFolderText() {
        val u = PydmgApp.prefs.romFolderUri
        textFolderValue.text = u?.lastPathSegment ?: "—"
    }

    private fun persistReadPermission(uri: Uri) {
        try {
            contentResolver.takePersistableUriPermission(
                uri, Intent.FLAG_GRANT_READ_URI_PERMISSION
            )
        } catch (_: Throwable) {}
    }

    private fun displayName(uri: Uri): String? {
        return DocumentFile.fromSingleUri(this, uri)?.name
            ?: DocumentFile.fromTreeUri(this, uri)?.name
    }

    private fun toast(text: String) {
        runOnUiThread { Toast.makeText(this, text, Toast.LENGTH_SHORT).show() }
    }

    companion object { private const val TAG = "pydmg-library" }
}

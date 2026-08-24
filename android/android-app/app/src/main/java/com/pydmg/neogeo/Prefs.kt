package com.pydmg.neogeo

import android.content.Context
import android.content.SharedPreferences
import android.net.Uri

/**
 * Tiny wrapper around SharedPreferences with strongly-typed getters/setters.
 * Lives behind [PydmgApp.prefs] so every activity sees the same instance.
 */
class Prefs(ctx: Context) {

    private val sp: SharedPreferences =
        ctx.applicationContext.getSharedPreferences(FILE, Context.MODE_PRIVATE)

    // ---------------- URIs ----------------
    var biosUri: Uri?
        get() = sp.getString(KEY_BIOS_URI, null)?.let(Uri::parse)
        set(v) { sp.edit().putString(KEY_BIOS_URI, v?.toString()).apply() }

    var biosLabel: String
        get() = sp.getString(KEY_BIOS_LABEL, "") ?: ""
        set(v) { sp.edit().putString(KEY_BIOS_LABEL, v).apply() }

    var romFolderUri: Uri?
        get() = sp.getString(KEY_ROM_FOLDER_URI, null)?.let(Uri::parse)
        set(v) { sp.edit().putString(KEY_ROM_FOLDER_URI, v?.toString()).apply() }

    var lastCartName: String
        get() = sp.getString(KEY_LAST_CART, "") ?: ""
        set(v) { sp.edit().putString(KEY_LAST_CART, v).apply() }

    // ---------------- Control prefs ----------------
    var useJoystick: Boolean
        get() = sp.getBoolean(KEY_USE_JOYSTICK, false)
        set(v) { sp.edit().putBoolean(KEY_USE_JOYSTICK, v).apply() }

    var controlOpacity: Float
        get() = sp.getFloat(KEY_OPACITY, 0.7f)
        set(v) { sp.edit().putFloat(KEY_OPACITY, v).apply() }

    var controlScale: Float
        get() = sp.getFloat(KEY_SCALE, 1.0f)
        set(v) { sp.edit().putFloat(KEY_SCALE, v).apply() }

    /** Vibración sutil al pulsar botones táctiles (on por defecto). */
    var hapticFeedback: Boolean
        get() = sp.getBoolean(KEY_HAPTICS, true)
        set(v) { sp.edit().putBoolean(KEY_HAPTICS, v).apply() }

    // ---------------- Video prefs ----------------
    var cropScreen: Boolean
        get() = sp.getBoolean(KEY_CROP, false)
        set(v) { sp.edit().putBoolean(KEY_CROP, v).apply() }

    var smoothFilter: Boolean
        get() = sp.getBoolean(KEY_SMOOTH, false)
        set(v) { sp.edit().putBoolean(KEY_SMOOTH, v).apply() }

    companion object {
        private const val FILE = "pydmg_neogeo_prefs"
        private const val KEY_BIOS_URI       = "bios_uri"
        private const val KEY_BIOS_LABEL     = "bios_label"
        private const val KEY_ROM_FOLDER_URI = "rom_folder_uri"
        private const val KEY_LAST_CART      = "last_cart_name"
        private const val KEY_USE_JOYSTICK   = "use_joystick"
        private const val KEY_OPACITY        = "control_opacity"
        private const val KEY_SCALE          = "control_scale"
        private const val KEY_HAPTICS        = "control_haptics"
        private const val KEY_CROP           = "video_crop"
        private const val KEY_SMOOTH         = "video_smooth"
    }
}

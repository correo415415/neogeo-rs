package com.pydmg.neogeo

import android.content.Context
import android.graphics.Bitmap
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.graphics.Rect
import android.util.AttributeSet
import android.view.SurfaceHolder
import android.view.SurfaceView

/**
 * Custom SurfaceView that renders the Neo Geo framebuffer.
 *
 * The Rust core hands us a 320×224 RGBA u32 array per frame
 * (`nativeGetFramebuffer`). We:
 *   1. Re-interpret it as Android `Color.argb` and feed into a
 *      reusable `Bitmap` (no per-frame allocations).
 *   2. Letterbox-blit the bitmap into the SurfaceView's canvas,
 *      preserving the 320:224 aspect ratio and using nearest-neighbour
 *      scaling for that crispy retro look.
 *
 * Rendering is push-driven: `MainActivity` calls `presentFrame` from
 * the emulator thread after each `nativeRunFrame()`.
 */
class EmulatorView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
    defStyleAttr: Int = 0
) : SurfaceView(context, attrs, defStyleAttr), SurfaceHolder.Callback {

    private val screenW = NativeBridge.nativeScreenWidth()    // 320
    private val screenH = NativeBridge.nativeScreenHeight()   // 224

    // Reusable framebuffer storage. Since v4-audio the Rust side
    // repacks pixels straight to ARGB_8888 (see
    // `nativeGetFramebufferArgb`), so this single buffer is what
    // `Bitmap.setPixels` consumes verbatim — no more Kotlin
    // per-pixel repacking loop that was costing 2–8 ms/frame on
    // mid-range devices.
    private val argbBuf = IntArray(screenW * screenH)
    private val bitmap = Bitmap.createBitmap(screenW, screenH, Bitmap.Config.ARGB_8888)

    private val bgPaint = Paint().apply { color = Color.BLACK }
    private val blitPaint = Paint().apply {
        isAntiAlias = false
        isFilterBitmap = false   // crispy nearest-neighbour by default
        isDither = false
    }

    /** Enables bilinear filtering for the next blits. Set from settings. */
    @Volatile var smoothFilter: Boolean = false
        set(value) {
            field = value
            blitPaint.isFilterBitmap = value
        }

    /**
     * If true, the 8-pixel border on each side of the 320×224 raster is
     * cropped at blit time, matching MAME's "Screen 0 Cropped (304×224)"
     * view. The native renderer always outputs 320×224 so this is a pure
     * presentation toggle.
     */
    @Volatile var cropScreen: Boolean = false
        set(value) {
            field = value
            // Recompute the destination rectangle the next time surfaceChanged
            // fires; until then a flag forces presentFrame to refresh it.
            needsRectRecompute = true
        }

    @Volatile private var surfaceReady = false
    @Volatile private var needsRectRecompute = false
    private val destRect = Rect()
    private val srcRect = Rect(0, 0, screenW, screenH)
    private var lastViewW = 0
    private var lastViewH = 0

    init {
        holder.addCallback(this)
        // Avoid Android compositor over-drawing our pixels.
        setZOrderMediaOverlay(false)
    }

    override fun surfaceCreated(holder: SurfaceHolder) {
        surfaceReady = true
    }

    override fun surfaceChanged(holder: SurfaceHolder, format: Int, width: Int, height: Int) {
        lastViewW = width; lastViewH = height
        computeDestRect(width, height)
    }

    override fun surfaceDestroyed(holder: SurfaceHolder) {
        surfaceReady = false
    }

    private fun computeDestRect(viewW: Int, viewH: Int) {
        // Pick the source sub-rectangle first (full vs cropped).
        if (cropScreen) {
            srcRect.set(8, 0, screenW - 8, screenH)
        } else {
            srcRect.set(0, 0, screenW, screenH)
        }
        val srcW = srcRect.width()
        val srcH = srcRect.height()
        val srcRatio = srcW.toFloat() / srcH.toFloat()
        val dstRatio = viewW.toFloat() / viewH.toFloat()
        if (dstRatio > srcRatio) {
            // Pillarbox (wider than source ratio → empty bars on the sides)
            val targetW = (viewH * srcRatio).toInt()
            val pad = (viewW - targetW) / 2
            destRect.set(pad, 0, pad + targetW, viewH)
        } else {
            // Letterbox (taller than source ratio → empty bars top/bottom)
            val targetH = (viewW / srcRatio).toInt()
            val pad = (viewH - targetH) / 2
            destRect.set(0, pad, viewW, pad + targetH)
        }
        needsRectRecompute = false
    }

    /**
     * Pull the latest framebuffer from native and blit it.
     * Called from the emulator thread.
     */
    fun presentFrame() {
        if (!surfaceReady) return
        // v4-audio: Rust hands us pixels already in `ARGB_8888` order
        // so we skip the per-pixel repacking loop entirely. Saves
        // 2–8 ms/frame on mid-range devices.
        if (!NativeBridge.nativeGetFramebufferArgb(argbBuf)) return
        bitmap.setPixels(argbBuf, 0, screenW, 0, 0, screenW, screenH)

        if (needsRectRecompute && lastViewW > 0 && lastViewH > 0) {
            computeDestRect(lastViewW, lastViewH)
        }

        val canvas: Canvas? = try { holder.lockCanvas() } catch (_: Throwable) { null }
        canvas ?: return
        try {
            canvas.drawRect(0f, 0f, canvas.width.toFloat(), canvas.height.toFloat(), bgPaint)
            canvas.drawBitmap(bitmap, srcRect, destRect, blitPaint)
        } finally {
            holder.unlockCanvasAndPost(canvas)
        }
    }
}

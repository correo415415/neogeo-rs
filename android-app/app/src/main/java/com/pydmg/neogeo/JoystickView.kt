package com.pydmg.neogeo

import android.content.Context
import android.graphics.Canvas
import android.graphics.Color
import android.graphics.Paint
import android.util.AttributeSet
import android.view.MotionEvent
import android.view.View
import kotlin.math.atan2
import kotlin.math.hypot
import kotlin.math.min

/**
 * Simple 8-way virtual joystick.
 *
 * It renders a translucent outer ring + inner knob. The user drags the
 * knob and the view reports a Neo Geo directional bitmask composed of
 * `BTN_UP/DOWN/LEFT/RIGHT` via [onDirectionMaskChanged]. Diagonals return
 * two bits (e.g. UP|RIGHT).
 */
class JoystickView @JvmOverloads constructor(
    context: Context,
    attrs: AttributeSet? = null,
    defStyleAttr: Int = 0
) : View(context, attrs, defStyleAttr) {

    var onDirectionMaskChanged: ((Int) -> Unit)? = null

    private val outerPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.argb(90, 35, 20, 10)
        style = Paint.Style.FILL
    }
    private val outerStroke = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.argb(100, 255, 171, 0)
        style = Paint.Style.STROKE
        strokeWidth = 4f
    }
    private val knobPaint = Paint(Paint.ANTI_ALIAS_FLAG).apply {
        color = Color.argb(165, 255, 171, 0)
        style = Paint.Style.FILL
    }

    private var cx = 0f
    private var cy = 0f
    private var radius = 0f
    private var knobRadius = 0f
    private var knobX = 0f
    private var knobY = 0f
    private var currentMask = 0

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) {
        cx = w / 2f
        cy = h / 2f
        radius = min(w, h) * 0.45f
        knobRadius = radius * 0.32f
        resetKnob()
    }

    override fun onDraw(canvas: Canvas) {
        super.onDraw(canvas)
        canvas.drawCircle(cx, cy, radius, outerPaint)
        canvas.drawCircle(cx, cy, radius, outerStroke)
        canvas.drawCircle(knobX, knobY, knobRadius, knobPaint)
    }

    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN,
            MotionEvent.ACTION_MOVE,
            MotionEvent.ACTION_POINTER_DOWN -> {
                updateFrom(event.x, event.y)
                return true
            }
            MotionEvent.ACTION_UP,
            MotionEvent.ACTION_CANCEL,
            MotionEvent.ACTION_POINTER_UP -> {
                resetKnob()
                setMask(0)
                invalidate()
                return true
            }
        }
        return super.onTouchEvent(event)
    }

    private fun updateFrom(x: Float, y: Float) {
        val dx = x - cx
        val dy = y - cy
        val dist = hypot(dx.toDouble(), dy.toDouble()).toFloat()
        val maxKnobTravel = radius - knobRadius
        val clamped = dist.coerceAtMost(maxKnobTravel)
        if (dist > 0.0001f) {
            val nx = dx / dist
            val ny = dy / dist
            knobX = cx + nx * clamped
            knobY = cy + ny * clamped
        } else {
            resetKnob()
        }

        // Dead-zone: 22 % of radius.
        if (dist < radius * 0.22f) {
            setMask(0)
        } else {
            val angle = atan2(dy, dx) // -PI..PI, 0 = right
            var mask = 0
            val deg = Math.toDegrees(angle.toDouble())
            when {
                deg >= -22.5 && deg < 22.5 -> mask = NativeBridge.BTN_RIGHT
                deg >= 22.5 && deg < 67.5 -> mask = NativeBridge.BTN_RIGHT or NativeBridge.BTN_DOWN
                deg >= 67.5 && deg < 112.5 -> mask = NativeBridge.BTN_DOWN
                deg >= 112.5 && deg < 157.5 -> mask = NativeBridge.BTN_LEFT or NativeBridge.BTN_DOWN
                deg >= 157.5 || deg < -157.5 -> mask = NativeBridge.BTN_LEFT
                deg >= -157.5 && deg < -112.5 -> mask = NativeBridge.BTN_LEFT or NativeBridge.BTN_UP
                deg >= -112.5 && deg < -67.5 -> mask = NativeBridge.BTN_UP
                deg >= -67.5 && deg < -22.5 -> mask = NativeBridge.BTN_UP or NativeBridge.BTN_RIGHT
            }
            setMask(mask)
        }
        invalidate()
    }

    private fun setMask(mask: Int) {
        if (mask == currentMask) return
        currentMask = mask
        onDirectionMaskChanged?.invoke(mask)
    }

    private fun resetKnob() {
        knobX = cx
        knobY = cy
    }
}

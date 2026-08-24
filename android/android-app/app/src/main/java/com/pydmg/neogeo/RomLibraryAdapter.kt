package com.pydmg.neogeo

import android.graphics.Color
import android.graphics.drawable.GradientDrawable
import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.TextView
import androidx.recyclerview.widget.DiffUtil
import androidx.recyclerview.widget.RecyclerView

/**
 * RecyclerView adapter for the ROM library list.
 *
 * Professional touches:
 *  - Real game titles / year / maker via [NeoGeoGames] (falls back to the
 *    zip name for unknown sets).
 *  - Per-game accent colour on the monogram tile (stable hash → hue), so
 *    the list reads like a proper game library instead of a file browser.
 *  - DiffUtil-based updates: filtering while typing only rebinds the rows
 *    that actually changed (no full-list flash, keeps scroll position).
 */
class RomLibraryAdapter(
    items: List<RomEntry>,
    private val onClick: (RomEntry) -> Unit,
) : RecyclerView.Adapter<RomLibraryAdapter.Vh>() {

    private var items: List<RomEntry> = items

    fun submit(list: List<RomEntry>) {
        val old = items
        val diff = DiffUtil.calculateDiff(object : DiffUtil.Callback() {
            override fun getOldListSize() = old.size
            override fun getNewListSize() = list.size
            override fun areItemsTheSame(a: Int, b: Int) =
                old[a].uri == list[b].uri
            override fun areContentsTheSame(a: Int, b: Int) =
                old[a] == list[b]
        })
        items = list
        diff.dispatchUpdatesTo(this)
    }

    override fun onCreateViewHolder(parent: ViewGroup, viewType: Int): Vh {
        val v = LayoutInflater.from(parent.context)
            .inflate(R.layout.item_rom, parent, false)
        return Vh(v)
    }

    override fun getItemCount(): Int = items.size

    override fun onBindViewHolder(holder: Vh, position: Int) {
        val item = items[position]
        val meta = NeoGeoGames.lookup(item.name)

        if (meta != null) {
            holder.title.text = meta.title
            holder.subtitle.text = if (item.isBios) {
                "BIOS / Parent set"
            } else {
                "${meta.year} · ${meta.maker} · ${formatSize(item.sizeBytes)}"
            }
        } else {
            holder.title.text = item.name
            holder.subtitle.text = if (item.isBios) "BIOS / Parent set"
            else "Cartucho · ${formatSize(item.sizeBytes)}"
        }

        val display = holder.title.text.toString()
        holder.letter.text = display.firstOrNull()?.uppercase() ?: "?"

        // Stable per-game accent: hash the set name to a hue, keep
        // saturation/brightness in the "muted neon over dark" range so every
        // tile harmonises with the amber/near-black theme.
        val hue = ((item.name.lowercase().hashCode() and 0x7FFFFFFF) % 360).toFloat()
        val accent = Color.HSVToColor(floatArrayOf(hue, 0.55f, 0.9f))
        val tileBg = Color.HSVToColor(46, floatArrayOf(hue, 0.7f, 0.5f))
        holder.letter.setTextColor(if (item.isBios) 0xFFFFAB00.toInt() else accent)
        val bg = holder.tileBg
        if (bg is GradientDrawable) {
            bg.setColor(if (item.isBios) 0x2EFFAB00 else tileBg)
        }

        holder.badge.visibility = if (item.isBios) View.VISIBLE else View.GONE
        holder.itemView.setOnClickListener { onClick(item) }
    }

    private fun formatSize(bytes: Long): String {
        if (bytes <= 0) return "tamaño desconocido"
        val mb = bytes / (1024.0 * 1024.0)
        return if (mb >= 1.0) "%.1f MB".format(mb)
        else "%.0f KB".format(bytes / 1024.0)
    }

    class Vh(v: View) : RecyclerView.ViewHolder(v) {
        val title: TextView    = v.findViewById(R.id.rom_title)
        val subtitle: TextView = v.findViewById(R.id.rom_subtitle)
        val letter: TextView   = v.findViewById(R.id.rom_thumb_letter)
        val badge: TextView    = v.findViewById(R.id.rom_badge)

        /**
         * Mutated copy of the tile background so per-row tints don't leak
         * into the shared drawable constant state.
         */
        val tileBg = (letter.parent as View).background?.mutate()
    }
}

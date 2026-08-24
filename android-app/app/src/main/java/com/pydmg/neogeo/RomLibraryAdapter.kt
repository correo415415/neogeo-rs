package com.pydmg.neogeo

import android.view.LayoutInflater
import android.view.View
import android.view.ViewGroup
import android.widget.TextView
import androidx.recyclerview.widget.RecyclerView

/**
 * RecyclerView adapter for the ROM library list. Each row shows a tile
 * (first letter), the title, a subtitle (size or "BIOS"), and an
 * optional badge.
 */
class RomLibraryAdapter(
    private var items: List<RomEntry>,
    private val onClick: (RomEntry) -> Unit,
) : RecyclerView.Adapter<RomLibraryAdapter.Vh>() {

    fun submit(list: List<RomEntry>) {
        items = list
        notifyDataSetChanged()
    }

    override fun onCreateViewHolder(parent: ViewGroup, viewType: Int): Vh {
        val v = LayoutInflater.from(parent.context)
            .inflate(R.layout.item_rom, parent, false)
        return Vh(v)
    }

    override fun getItemCount(): Int = items.size

    override fun onBindViewHolder(holder: Vh, position: Int) {
        val item = items[position]
        holder.title.text = item.name
        holder.letter.text = item.name.firstOrNull()?.uppercase() ?: "?"
        holder.subtitle.text = if (item.isBios) "BIOS / Parent set"
        else "Cartridge zip — ${formatSize(item.sizeBytes)}"
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
        val title: TextView   = v.findViewById(R.id.rom_title)
        val subtitle: TextView = v.findViewById(R.id.rom_subtitle)
        val letter: TextView  = v.findViewById(R.id.rom_thumb_letter)
        val badge: TextView   = v.findViewById(R.id.rom_badge)
    }
}

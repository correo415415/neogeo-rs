package com.pydmg.neogeo

/**
 * Static metadata for well-known Neo Geo ROM sets so the library can show
 * proper game titles ("Metal Slug 3 · 2000 · SNK") instead of raw zip
 * names ("mslug3"). Unknown sets gracefully fall back to the file name.
 *
 * The key is the canonical MAME set name (zip base name, lowercase).
 */
object NeoGeoGames {

    data class Meta(val title: String, val year: String, val maker: String)

    private val db: Map<String, Meta> = mapOf(
        // --- Metal Slug ---
        "mslug" to Meta("Metal Slug", "1996", "Nazca"),
        "mslug2" to Meta("Metal Slug 2", "1998", "SNK"),
        "mslugx" to Meta("Metal Slug X", "1999", "SNK"),
        "mslug3" to Meta("Metal Slug 3", "2000", "SNK"),
        "mslug4" to Meta("Metal Slug 4", "2002", "Mega / Playmore"),
        "mslug5" to Meta("Metal Slug 5", "2003", "SNK Playmore"),
        // --- The King of Fighters ---
        "kof94" to Meta("The King of Fighters '94", "1994", "SNK"),
        "kof95" to Meta("The King of Fighters '95", "1995", "SNK"),
        "kof96" to Meta("The King of Fighters '96", "1996", "SNK"),
        "kof97" to Meta("The King of Fighters '97", "1997", "SNK"),
        "kof98" to Meta("The King of Fighters '98", "1998", "SNK"),
        "kof99" to Meta("The King of Fighters '99", "1999", "SNK"),
        "kof2000" to Meta("The King of Fighters 2000", "2000", "SNK"),
        "kof2001" to Meta("The King of Fighters 2001", "2001", "Eolith / SNK"),
        "kof2002" to Meta("The King of Fighters 2002", "2002", "Eolith / Playmore"),
        "kof2003" to Meta("The King of Fighters 2003", "2003", "SNK Playmore"),
        // --- Samurai Shodown ---
        "samsho" to Meta("Samurai Shodown", "1993", "SNK"),
        "samsho2" to Meta("Samurai Shodown II", "1994", "SNK"),
        "samsho3" to Meta("Samurai Shodown III", "1995", "SNK"),
        "samsho4" to Meta("Samurai Shodown IV", "1996", "SNK"),
        "samsho5" to Meta("Samurai Shodown V", "2003", "Yuki / SNK Playmore"),
        "samsh5sp" to Meta("Samurai Shodown V Special", "2004", "Yuki / SNK Playmore"),
        // --- Fatal Fury / Garou ---
        "fatfury1" to Meta("Fatal Fury", "1991", "SNK"),
        "fatfury2" to Meta("Fatal Fury 2", "1992", "SNK"),
        "fatfursp" to Meta("Fatal Fury Special", "1993", "SNK"),
        "fatfury3" to Meta("Fatal Fury 3", "1995", "SNK"),
        "rbff1" to Meta("Real Bout Fatal Fury", "1995", "SNK"),
        "rbffspec" to Meta("Real Bout Fatal Fury Special", "1996", "SNK"),
        "rbff2" to Meta("Real Bout Fatal Fury 2", "1998", "SNK"),
        "garou" to Meta("Garou: Mark of the Wolves", "1999", "SNK"),
        // --- Art of Fighting ---
        "aof" to Meta("Art of Fighting", "1992", "SNK"),
        "aof2" to Meta("Art of Fighting 2", "1994", "SNK"),
        "aof3" to Meta("Art of Fighting 3", "1996", "SNK"),
        // --- Last Blade ---
        "lastblad" to Meta("The Last Blade", "1997", "SNK"),
        "lastbld2" to Meta("The Last Blade 2", "1998", "SNK"),
        // --- Shooters / action / misc ---
        "pulstar" to Meta("Pulstar", "1995", "Aicom"),
        "blazstar" to Meta("Blazing Star", "1998", "Yumekobo"),
        "shocktro" to Meta("Shock Troopers", "1997", "Saurus"),
        "shocktr2" to Meta("Shock Troopers: 2nd Squad", "1998", "Saurus"),
        "twinspri" to Meta("Twinkle Star Sprites", "1996", "ADK"),
        "neobombe" to Meta("Neo Bomberman", "1997", "Hudson"),
        "turfmast" to Meta("Neo Turf Masters", "1996", "Nazca"),
        "windjamm" to Meta("Windjammers", "1994", "Data East"),
        "wjammers" to Meta("Windjammers", "1994", "Data East"),
        "sengoku3" to Meta("Sengoku 3", "2001", "Noise Factory / SNK"),
        "nitd" to Meta("Nightmare in the Dark", "2000", "Eleven / Gavaking"),
        "ganryu" to Meta("Ganryu", "1999", "Visco"),
        "s1945p" to Meta("Strikers 1945 Plus", "1999", "Psikyo"),
        "prehisle" to Meta("Prehistoric Isle 2", "1999", "Yumekobo"),
        "magdrop3" to Meta("Magical Drop III", "1997", "Data East"),
        "pbobblen" to Meta("Puzzle Bobble", "1994", "Taito"),
        "pbobbl2n" to Meta("Puzzle Bobble 2", "1999", "Taito / SNK"),
        "mutnat" to Meta("Mutation Nation", "1992", "SNK"),
        "cyberlip" to Meta("Cyber-Lip", "1990", "SNK"),
        "nam1975" to Meta("NAM-1975", "1990", "SNK"),
        "spinmast" to Meta("Spin Master", "1993", "Data East"),
        "kabukikl" to Meta("Far East of Eden: Kabuki Klash", "1995", "Hudson"),
        "viewpoin" to Meta("Viewpoint", "1992", "Sammy / Aicom"),
        "svc" to Meta("SNK vs. Capcom: SVC Chaos", "2003", "SNK Playmore"),
        "rotd" to Meta("Rage of the Dragons", "2002", "Evoga / Noise Factory"),
        "matrim" to Meta("Matrimelee", "2002", "Noise Factory / Atlus"),
        "sonicwi2" to Meta("Aero Fighters 2", "1994", "Video System"),
        "sonicwi3" to Meta("Aero Fighters 3", "1995", "Video System"),
        "stakwin" to Meta("Stakes Winner", "1995", "Saurus"),
        "ssideki" to Meta("Super Sidekicks", "1992", "SNK"),
        "ssideki2" to Meta("Super Sidekicks 2", "1994", "SNK"),
        "ssideki3" to Meta("Super Sidekicks 3", "1995", "SNK"),
        "ssideki4" to Meta("The Ultimate 11", "1996", "SNK"),
        "kizuna" to Meta("Kizuna Encounter", "1996", "SNK"),
        "wakuwak7" to Meta("Waku Waku 7", "1996", "Sunsoft"),
        "breakers" to Meta("Breakers", "1996", "Visco"),
        "breakrev" to Meta("Breakers Revenge", "1998", "Visco"),
        "karnovr" to Meta("Karnov's Revenge", "1994", "Data East"),
        "gowcaizr" to Meta("Voltage Fighter Gowcaizer", "1995", "Technos"),
        "sengoku" to Meta("Sengoku", "1991", "SNK"),
        "sengoku2" to Meta("Sengoku 2", "1993", "SNK"),
        "burningf" to Meta("Burning Fight", "1991", "SNK"),
        "kotm" to Meta("King of the Monsters", "1991", "SNK"),
        "kotm2" to Meta("King of the Monsters 2", "1992", "SNK"),
        "lresort" to Meta("Last Resort", "1992", "SNK"),
        "eightman" to Meta("Eight Man", "1991", "SNK / Pallas"),
        "superspy" to Meta("The Super Spy", "1990", "SNK"),
        "alpham2" to Meta("Alpha Mission II", "1991", "SNK"),
        "ncombat" to Meta("Ninja Combat", "1990", "ADK"),
        "ncommand" to Meta("Ninja Commando", "1992", "ADK"),
        "crsword" to Meta("Crossed Swords", "1991", "ADK"),
        "trally" to Meta("Thrash Rally", "1991", "ADK"),
        "wh1" to Meta("World Heroes", "1992", "ADK"),
        "wh2" to Meta("World Heroes 2", "1993", "ADK"),
        "wh2j" to Meta("World Heroes 2 Jet", "1994", "ADK"),
        "whp" to Meta("World Heroes Perfect", "1995", "ADK"),
        "aodk" to Meta("Aggressors of Dark Kombat", "1994", "ADK"),
        "overtop" to Meta("OverTop", "1996", "ADK"),
        "ninjamas" to Meta("Ninja Master's", "1996", "ADK"),
        "strhoop" to Meta("Street Hoop", "1994", "Data East"),
        "ghostlop" to Meta("Ghostlop", "1996", "Data East"),
        "goalx3" to Meta("Goal! Goal! Goal!", "1995", "Visco"),
        "neodrift" to Meta("Neo Drift Out", "1996", "Visco"),
        "neomrdo" to Meta("Neo Mr. Do!", "1996", "Visco"),
        "puzzledp" to Meta("Puzzle De Pon!", "1995", "Taito / Visco"),
        "flipshot" to Meta("Battle Flip Shot", "1998", "Visco"),
        "ctomaday" to Meta("Captain Tomaday", "1999", "Visco"),
        "androdun" to Meta("Andro Dunos", "1992", "Visco"),
        "bjourney" to Meta("Blue's Journey", "1990", "ADK"),
        "maglord" to Meta("Magician Lord", "1990", "ADK"),
        "joyjoy" to Meta("Puzzled", "1990", "SNK"),
        "marukodq" to Meta("Chibi Marukochan Deluxe Quiz", "1995", "Takara"),
        "tophuntr" to Meta("Top Hunter", "1994", "SNK"),
        "savagere" to Meta("Savage Reign", "1995", "SNK"),
        "gpilots" to Meta("Ghost Pilots", "1991", "SNK"),
        "3countb" to Meta("3 Count Bout", "1993", "SNK"),
        "tws96" to Meta("Tecmo World Soccer '96", "1996", "Tecmo"),
        "fightfev" to Meta("Fight Fever", "1994", "Viccom"),
        "galaxyfg" to Meta("Galaxy Fight", "1995", "Sunsoft"),
        "pspikes2" to Meta("Power Spikes II", "1994", "Video System"),
        "zedblade" to Meta("Zed Blade", "1994", "NMK"),
        "zupapa" to Meta("Zupapa!", "2001", "SNK"),
        "bangbead" to Meta("Bang Bead", "2000", "Visco"),
        "irrmaze" to Meta("The Irritating Maze", "1997", "SNK / Saurus"),
        "popbounc" to Meta("Pop 'n Bounce", "1997", "Video System"),
        "gururin" to Meta("Gururin", "1994", "Face"),
        "panicbom" to Meta("Panic Bomber", "1994", "Hudson / Eighting"),
        "pgoal" to Meta("Pleasure Goal", "1996", "Saurus"),
        "quizdais" to Meta("Quiz Daisousa Sen", "1991", "SNK"),
        "socbrawl" to Meta("Soccer Brawl", "1991", "SNK"),
        "fbfrenzy" to Meta("Football Frenzy", "1992", "SNK"),
        "bstars" to Meta("Baseball Stars Professional", "1990", "SNK"),
        "bstars2" to Meta("Baseball Stars 2", "1992", "SNK"),
        "2020bb" to Meta("2020 Super Baseball", "1991", "SNK / Pallas"),
        "tpgolf" to Meta("Top Player's Golf", "1990", "SNK"),
        "legendos" to Meta("Legend of Success Joe", "1991", "SNK / Wave"),
        "roboarmy" to Meta("Robo Army", "1991", "SNK"),
        "janshin" to Meta("Jyanshin Densetsu", "1994", "Aicom"),
        "pnyaa" to Meta("Pochi and Nyaa", "2003", "Aiky / Taito"),
        "jockeygp" to Meta("Jockey Grand Prix", "2001", "Sun Amusement"),
        "vliner" to Meta("V-Liner", "2001", "Dyna / BrezzaSoft"),
        "doubledr" to Meta("Double Dragon", "1995", "Technos"),
        "sdodgeb" to Meta("Super Dodge Ball", "1996", "Technos"),
        "moshougi" to Meta("Master of Syougi", "1995", "ADK"),
        "quizkof" to Meta("Quiz King of Fighters", "1995", "Saurus"),
    )

    /** Suffix-stripped lookup: `mslug3h`, `kof98n`, `garouo` → base set. */
    fun lookup(zipBaseName: String): Meta? {
        val key = zipBaseName.lowercase()
        db[key]?.let { return it }
        // Common regional/revision suffixes on clone sets.
        for (suffix in listOf("h", "n", "o", "a", "b", "d", "p", "k", "bl")) {
            if (key.endsWith(suffix) && key.length > suffix.length) {
                db[key.dropLast(suffix.length)]?.let { return it }
            }
        }
        return null
    }
}

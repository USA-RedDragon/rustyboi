package dev.mcswain.rustyboi

import java.io.InputStream
import java.util.zip.CRC32
import java.util.zip.ZipInputStream

/**
 * Pure, JVM-only ROM-library helpers extracted from [RustyboiActivity] so they
 * can be unit-tested without the Android framework (no Context/ContentResolver/
 * Uri). [RustyboiActivity] delegates to these; there is no logic duplication.
 */
object RomScan {

    /**
     * ROM-image extensions, lowercase, matched as name suffixes. The single
     * source of truth on this side of the language boundary — [isRomFile] and
     * [crcOfRomStream] must both use it.
     *
     * KEEP IN SYNC with `EXTS` in `rustyboi-session/src/rom_zip.rs` (the list
     * cannot be shared across the language boundary).
     */
    private val ROM_EXTS = listOf(".gb", ".gbc", ".sgb")

    private fun String.hasRomExt(): Boolean {
        val lower = lowercase()
        return ROM_EXTS.any { lower.endsWith(it) }
    }

    /** Extension classification for ROM-library scanning (case-insensitive). */
    fun isRomFile(name: String): Boolean =
        name.hasRomExt() || name.lowercase().endsWith(".zip")

    /**
     * Strip the outermost extension. `Pokemon Crystal.zip` and
     * `Pokemon Crystal.gbc` both → `Pokemon Crystal`. A leading dot or no dot
     * leaves the name unchanged (so the `.sav` stem is never empty).
     */
    fun savStem(displayName: String): String {
        val dot = displayName.lastIndexOf('.')
        return if (dot <= 0) displayName else displayName.substring(0, dot)
    }

    /** CRC32 of a stream, read to completion. Does not close the stream. */
    fun crcOfStream(input: InputStream): Long = sizedCrcOfStream(input).second

    /** [crcOfStream] plus the byte count consumed, as `(size, crc)`. */
    private fun sizedCrcOfStream(input: InputStream): Pair<Long, Long> {
        val crc = CRC32()
        val buf = ByteArray(65536)
        var size = 0L
        var n = input.read(buf)
        while (n >= 0) {
            crc.update(buf, 0, n)
            size += n
            n = input.read(buf)
        }
        return size to crc.value
    }

    /**
     * CRC32 over a ROM stream, matching No-Intro's checksum of the raw ROM
     * image (of the uncompressed ROM, not the archive).
     *
     * KEEP IN SYNC with `extract_rom` / `extract_from_zip` in
     * `rustyboi-session/src/rom_zip.rs`: this must hash *exactly* the entry the
     * Rust loader extracts and runs, otherwise a perfectly playable zip is
     * stored under a CRC that matches nothing in the No-Intro database. That
     * loader's selection rule, reproduced here:
     * 1. entries are visited in archive order; directory entries are skipped;
     * 2. the first entry whose lowercased name ends in [ROM_EXTS] wins outright;
     * 3. with no such entry, the largest by uncompressed size wins, ties going
     *    to the earlier entry;
     * 4. if the winner is empty (or the archive is unreadable/has no entries),
     *    Rust extracts nothing and passes the raw archive to the cartridge
     *    loader, which rejects it — there is no ROM to identify, so 0 is
     *    returned to mean "unidentified".
     *
     * Two representation details, both benign for well-formed archives:
     * `ZipInputStream` walks local headers in physical order while Rust walks the
     * central directory (writers emit both in the same order), and sizes are
     * counted while decompressing rather than taken from the entry header, which
     * `ZipInputStream` may report as -1.
     */
    fun crcOfRomStream(input: InputStream, name: String): Long {
        if (!name.lowercase().endsWith(".zip")) return crcOfStream(input)
        ZipInputStream(input).use { zis ->
            var largest = 0L to 0L // (size, crc) of the largest entry so far
            var e = zis.nextEntry
            while (e != null) {
                // Rust's zip crate also counts a trailing '\' as a directory;
                // ZipEntry.isDirectory only checks '/'.
                if (!e.isDirectory && !e.name.endsWith("\\")) {
                    if (e.name.hasRomExt()) return crcOfStream(zis)
                    val entry = sizedCrcOfStream(zis)
                    if (entry.first > largest.first) largest = entry
                }
                e = zis.nextEntry
            }
            return largest.second
        }
    }

    /** [crcOfRomStream] over an in-memory ROM image. */
    fun crcOfRomBytes(bytes: ByteArray, name: String): Long =
        crcOfRomStream(bytes.inputStream(), name)
}

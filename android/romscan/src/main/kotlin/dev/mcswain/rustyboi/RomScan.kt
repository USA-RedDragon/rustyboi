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

    /** Extension classification for ROM-library scanning (case-insensitive). */
    fun isRomFile(name: String): Boolean {
        val lower = name.lowercase()
        return lower.endsWith(".gb") ||
            lower.endsWith(".gbc") ||
            lower.endsWith(".sgb") ||
            lower.endsWith(".zip")
    }

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
    fun crcOfStream(input: InputStream): Long {
        val crc = CRC32()
        val buf = ByteArray(65536)
        var n = input.read(buf)
        while (n >= 0) {
            crc.update(buf, 0, n)
            n = input.read(buf)
        }
        return crc.value
    }

    /**
     * CRC32 over a ROM stream, matching No-Intro's checksum of the raw ROM
     * image. For a `.zip`, hashes the first contained `.gb`/`.gbc`/`.sgb` entry
     * (No-Intro CRCs are of the uncompressed ROM, not the archive), skipping
     * directories and non-ROM entries. Returns 0 if a zip has no ROM entry.
     */
    fun crcOfRomStream(input: InputStream, name: String): Long {
        return if (name.lowercase().endsWith(".zip")) {
            ZipInputStream(input).use { zis ->
                var result = 0L
                var e = zis.nextEntry
                while (e != null) {
                    val en = e.name.lowercase()
                    if (!e.isDirectory &&
                        (en.endsWith(".gb") || en.endsWith(".gbc") || en.endsWith(".sgb"))
                    ) {
                        result = crcOfStream(zis)
                        break
                    }
                    e = zis.nextEntry
                }
                result
            }
        } else {
            crcOfStream(input)
        }
    }

    /** [crcOfRomStream] over an in-memory ROM image. */
    fun crcOfRomBytes(bytes: ByteArray, name: String): Long =
        crcOfRomStream(bytes.inputStream(), name)
}

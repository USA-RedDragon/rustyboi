package dev.mcswain.rustyboi

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import java.io.ByteArrayOutputStream
import java.util.zip.CRC32
import java.util.zip.ZipEntry
import java.util.zip.ZipOutputStream

class RomScanTest {

    // ---- isRomFile ----------------------------------------------------------

    @Test
    fun isRomFile_acceptsRomExtensions() {
        assertTrue(RomScan.isRomFile("game.gb"))
        assertTrue(RomScan.isRomFile("game.gbc"))
        assertTrue(RomScan.isRomFile("game.sgb"))
        assertTrue(RomScan.isRomFile("archive.zip"))
    }

    @Test
    fun isRomFile_isCaseInsensitive() {
        assertTrue(RomScan.isRomFile("GAME.GB"))
        assertTrue(RomScan.isRomFile("Game.GbC"))
        assertTrue(RomScan.isRomFile("Pokemon Crystal.ZIP"))
    }

    @Test
    fun isRomFile_rejectsNonRom() {
        assertFalse(RomScan.isRomFile("readme.txt"))
        assertFalse(RomScan.isRomFile("save.sav"))
        assertFalse(RomScan.isRomFile("noextension"))
        assertFalse(RomScan.isRomFile("game.gba"))
        assertFalse(RomScan.isRomFile(""))
        // Substring, not extension: must anchor on the trailing dot.
        assertFalse(RomScan.isRomFile("gb"))
        assertFalse(RomScan.isRomFile("mygbfile"))
    }

    // ---- savStem ------------------------------------------------------------

    @Test
    fun savStem_stripsOutermostExtension() {
        assertEquals("Pokemon Crystal", RomScan.savStem("Pokemon Crystal.zip"))
        assertEquals("Pokemon Crystal", RomScan.savStem("Pokemon Crystal.gbc"))
        assertEquals("game", RomScan.savStem("game.gb"))
    }

    @Test
    fun savStem_stripsOnlyTheLastExtension() {
        assertEquals("rom.tar", RomScan.savStem("rom.tar.gz"))
    }

    @Test
    fun savStem_noExtensionUnchanged() {
        assertEquals("noextension", RomScan.savStem("noextension"))
    }

    @Test
    fun savStem_leadingDotUnchanged() {
        // A leading dot (index 0) must not blank the stem.
        assertEquals(".gitignore", RomScan.savStem(".gitignore"))
        assertEquals(".gbc", RomScan.savStem(".gbc"))
    }

    // ---- crcOfStream --------------------------------------------------------

    @Test
    fun crcOfStream_matchesJavaCrc32() {
        val data = "The quick brown fox".toByteArray()
        val expected = CRC32().apply { update(data) }.value
        assertEquals(expected, RomScan.crcOfStream(data.inputStream()))
    }

    @Test
    fun crcOfStream_emptyIsZeroCrc() {
        // CRC32 of the empty stream is 0 (the CRC32 seed with no updates).
        assertEquals(0L, RomScan.crcOfStream(ByteArray(0).inputStream()))
    }

    @Test
    fun crcOfStream_largeMultiChunk() {
        // Exceed the 64 KiB read buffer to exercise the multi-read loop.
        val data = ByteArray(200_000) { (it * 31 + 7).toByte() }
        val expected = CRC32().apply { update(data) }.value
        assertEquals(expected, RomScan.crcOfStream(data.inputStream()))
    }

    // ---- crcOfRomBytes: raw -------------------------------------------------

    @Test
    fun crcOfRomBytes_rawMatchesRawCrc() {
        val rom = ByteArray(1024) { it.toByte() }
        val expected = CRC32().apply { update(rom) }.value
        assertEquals(expected, RomScan.crcOfRomBytes(rom, "game.gb"))
        assertEquals(expected, RomScan.crcOfRomBytes(rom, "game.gbc"))
    }

    // ---- crcOfRomBytes: zip -------------------------------------------------

    @Test
    fun crcOfRomBytes_zipPicksFirstRomEntry() {
        val romData = ByteArray(512) { (it xor 0x5A).toByte() }
        val zip = buildZip(
            "notes.txt" to "hello".toByteArray(),
            "game.gbc" to romData,
            "other.gb" to ByteArray(16) { 0xFF.toByte() },
        )
        val expected = CRC32().apply { update(romData) }.value
        assertEquals(expected, RomScan.crcOfRomBytes(zip, "bundle.zip"))
    }

    @Test
    fun crcOfRomBytes_zipSkipsNonRomAndDirEntries() {
        val romData = ByteArray(300) { (it + 3).toByte() }
        val zip = buildZip(
            "sub/" to null,                       // directory entry
            "sub/readme.md" to "x".toByteArray(), // non-ROM
            "sub/game.sgb" to romData,            // first ROM
        )
        val expected = CRC32().apply { update(romData) }.value
        assertEquals(expected, RomScan.crcOfRomBytes(zip, "bundle.ZIP"))
    }

    @Test
    fun crcOfRomBytes_zipPicksRomEntryCaseInsensitively() {
        val romData = ByteArray(64) { (it + 9).toByte() }
        val zip = buildZip(
            "data.bin" to ByteArray(4096),  // larger, but the ROM extension wins
            "GAME.GBC" to romData,
        )
        val expected = CRC32().apply { update(romData) }.value
        assertEquals(expected, RomScan.crcOfRomBytes(zip, "bundle.zip"))
    }

    // ---- crcOfRomBytes: largest-entry fallback ------------------------------
    // These mirror the Rust tests in rustyboi-session/src/rom_zip.rs; with no
    // ROM-extension entry the Rust loader plays the largest entry, so the CRC
    // must be of that entry (0 here would break No-Intro matching for a zip
    // that plays perfectly well).

    @Test
    fun crcOfRomBytes_zipWithNoRomEntryFallsBackToLargest() {
        val big = ByteArray(4096) { (it xor 0x33).toByte() }
        val zip = buildZip(
            "readme.txt" to "nothing here".toByteArray(),
            "cover.png" to ByteArray(8),
            "rom.bin" to big,
        )
        val expected = CRC32().apply { update(big) }.value
        assertEquals(expected, RomScan.crcOfRomBytes(zip, "bundle.zip"))
    }

    @Test
    fun crcOfRomBytes_largestFallbackBreaksTiesTowardEarlierEntry() {
        val first = ByteArray(1024) { 0x11 }
        val second = ByteArray(1024) { 0x22 } // same size, later → loses
        val zip = buildZip("a.bin" to first, "b.bin" to second)
        val expected = CRC32().apply { update(first) }.value
        assertEquals(expected, RomScan.crcOfRomBytes(zip, "bundle.zip"))
    }

    @Test
    fun crcOfRomBytes_largestFallbackIgnoresDirectoryEntries() {
        val data = ByteArray(64) { (it * 7).toByte() }
        val zip = buildZip(
            "a-very-long-directory-name/" to null,
            "a-very-long-directory-name/x.bin" to data,
        )
        val expected = CRC32().apply { update(data) }.value
        assertEquals(expected, RomScan.crcOfRomBytes(zip, "bundle.zip"))
    }

    @Test
    fun crcOfRomBytes_zipWithOnlyEmptyEntriesIsZero() {
        // Nothing to extract: Rust hands the raw archive to the cartridge
        // loader, which rejects it, so there is no ROM to identify.
        val zip = buildZip("empty.dat" to ByteArray(0), "sub/" to null)
        assertEquals(0L, RomScan.crcOfRomBytes(zip, "bundle.zip"))
    }

    @Test
    fun crcOfRomBytes_garbageZipNamedZipIsZero() {
        // Not a valid archive; the reader yields no entries → 0, no throw.
        val garbage = ByteArray(64) { 0xAB.toByte() }
        assertEquals(0L, RomScan.crcOfRomBytes(garbage, "broken.zip"))
    }

    /** Build an in-memory zip. A null payload creates a directory entry. */
    private fun buildZip(vararg entries: Pair<String, ByteArray?>): ByteArray {
        val bos = ByteArrayOutputStream()
        ZipOutputStream(bos).use { zos ->
            for ((name, payload) in entries) {
                zos.putNextEntry(ZipEntry(name))
                if (payload != null) zos.write(payload)
                zos.closeEntry()
            }
        }
        return bos.toByteArray()
    }
}

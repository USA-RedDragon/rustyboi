package dev.mcswain.rustyboi

import android.content.Intent
import android.net.Uri
import android.view.InputDevice
import android.view.MotionEvent
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.util.Log
import android.widget.Toast
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
import androidx.annotation.Keep
import androidx.documentfile.provider.DocumentFile
import com.google.androidgamesdk.GameActivity
import org.json.JSONArray
import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.IOException
import java.util.ArrayDeque
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors

/**
 * Subclass of [GameActivity] that wires the Storage Access Framework
 * (SAF) ROM picker and ROM-library tree picker into the native Rust
 * event loop.
 *
 * Three SAF flows are exposed to native via JNI:
 *  - [pickRomFromSaf] — single-document `OpenDocument` picker.
 *    Returns ROM bytes + display name. Battery `.sav` is NOT
 *    persisted on this path (per-document grants can't reliably open
 *    writable siblings).
 *  - [pickLibraryTree] — `OpenDocumentTree` picker that grants us a
 *    persistable tree URI. Used to register a ROM-library root.
 *  - [scanLibrary] / [loadRomEntry] — operate on the tree URI:
 *    enumerate ROMs recursively, and load a chosen ROM along with a
 *    writable sibling `.sav` file descriptor.
 */
// R8 (release `isMinifyEnabled`) can't see the JNI boundary: the SAF up-calls
// (pickRomFromSaf/pickLibraryTree/scanLibrary/loadRomEntry/showToast) are reached
// only from Rust via `env.call_method`, and the `native` down-calls are linked by
// the mangled `Java_dev_mcswain_rustyboi_RustyboiActivity_*` symbol. Keep the whole
// class (name + members) so shrinking/obfuscation can't strip or rename any of it.
@Keep
class RustyboiActivity : GameActivity() {

    /** Single-threaded background reader so multiple picks serialize. */
    private val ioExecutor: ExecutorService = Executors.newSingleThreadExecutor()

    /**
     * Run [body] on [ioExecutor]. `submit`'s Future is discarded, so any
     * Throwable escaping the lambda would be captured by it silently and the
     * native side would wait forever for its completion callback. Catch
     * everything, log, and fire [onFailure] so native always hears back.
     */
    private fun submitLogged(name: String, onFailure: () -> Unit, body: () -> Unit) {
        ioExecutor.submit {
            try {
                body()
            } catch (t: Throwable) {
                Log.e(TAG, "$name failed", t)
                onFailure()
            }
        }
    }

    private lateinit var pickRomLauncher: ActivityResultLauncher<Array<String>>
    private lateinit var pickTreeLauncher: ActivityResultLauncher<Uri?>

    override fun onCreate(savedInstanceState: android.os.Bundle?) {
        pickRomLauncher = registerForActivityResult(
            ActivityResultContracts.OpenDocument()
        ) { uri: Uri? ->
            if (uri == null) {
                nativeOnRomPickCancelled()
                return@registerForActivityResult
            }
            submitLogged("pickRom", onFailure = { nativeOnRomPickCancelled() }) {
                val name = queryDisplayName(uri)
                val bytes = readAllBytes(uri)
                if (bytes == null) {
                    nativeOnRomPickCancelled()
                } else {
                    nativeOnRomPicked(bytes, name ?: "rom.gb")
                }
            }
        }
        pickTreeLauncher = registerForActivityResult(
            ActivityResultContracts.OpenDocumentTree()
        ) { uri: Uri? ->
            if (uri == null) {
                nativeOnTreePicked("")
                return@registerForActivityResult
            }
            try {
                val flags = Intent.FLAG_GRANT_READ_URI_PERMISSION or
                        Intent.FLAG_GRANT_WRITE_URI_PERMISSION
                contentResolver.takePersistableUriPermission(uri, flags)
            } catch (e: SecurityException) {
                Log.w(TAG, "takePersistableUriPermission failed", e)
            }
            nativeOnTreePicked(uri.toString())
        }
        super.onCreate(savedInstanceState)
    }

    /**
     * Gamepad input diagnostics + analog-axis forwarding. GameActivity routes
     * input to native (winit) internally; winit delivers key events but DROPS
     * analog motion. winit's Android backend drops joystick motion events, so we
     * hook the earliest activity-level dispatch and forward the axes to Rust via
     * JNI. Sticks + hat come as X/Y/Z/RZ/HAT; the L2/R2 analog triggers come as
     * separate axes (LTRIGGER/RTRIGGER, or BRAKE/GAS on some pads) — forward the
     * max of each pair so triggers register regardless of the controller.
     */
    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        val joystick = event.source and InputDevice.SOURCE_JOYSTICK ==
            InputDevice.SOURCE_JOYSTICK
        if (joystick) {
            val lt = maxOf(
                event.getAxisValue(MotionEvent.AXIS_LTRIGGER),
                event.getAxisValue(MotionEvent.AXIS_BRAKE),
            )
            val rt = maxOf(
                event.getAxisValue(MotionEvent.AXIS_RTRIGGER),
                event.getAxisValue(MotionEvent.AXIS_GAS),
            )
            nativeOnGamepadAxes(
                event.getAxisValue(MotionEvent.AXIS_X),
                event.getAxisValue(MotionEvent.AXIS_Y),
                event.getAxisValue(MotionEvent.AXIS_Z),
                event.getAxisValue(MotionEvent.AXIS_RZ),
                event.getAxisValue(MotionEvent.AXIS_HAT_X),
                event.getAxisValue(MotionEvent.AXIS_HAT_Y),
                lt,
                rt,
            )
        }
        return super.dispatchGenericMotionEvent(event)
    }

    /** Called from JNI to launch the single-document SAF picker. */
    @Suppress("unused")
    fun pickRomFromSaf() {
        runOnUiThread {
            try {
                pickRomLauncher.launch(arrayOf("*/*"))
            } catch (e: Exception) {
                Log.e(TAG, "Failed to launch SAF picker", e)
                nativeOnRomPickCancelled()
            }
        }
    }

    /** Called from JNI to launch the SAF tree picker (ROM library root). */
    @Suppress("unused")
    fun pickLibraryTree() {
        runOnUiThread {
            try {
                pickTreeLauncher.launch(null)
            } catch (e: Exception) {
                Log.e(TAG, "Failed to launch SAF tree picker", e)
                nativeOnTreePicked("")
            }
        }
    }

    /**
     * Display a short system Toast. Called from JNI on Android in
     * place of the egui status bar; native code passes the message
     * via [showToast] and Android handles duration / animation.
     */
    @Suppress("unused")
    fun showToast(message: String) {
        runOnUiThread {
            try {
                Toast.makeText(this, message, Toast.LENGTH_SHORT).show()
            } catch (e: Exception) {
                Log.w(TAG, "showToast failed", e)
            }
        }
    }

    /**
     * Recursively walk the tree at [treeUriString] looking for ROM files
     * (`.gb`, `.gbc`, `.sgb`, `.zip`). Reports the resulting list back
     * to native via [nativeOnLibraryScanResult] as a JSON array. Runs
     * on [ioExecutor]; safe to call from the UI thread.
     */
    @Suppress("unused")
    fun scanLibrary(treeUriString: String) {
        submitLogged("scanLibrary", onFailure = { nativeOnLibraryScanResult("") }) {
            val treeUri = try {
                Uri.parse(treeUriString)
            } catch (e: Exception) {
                Log.w(TAG, "scanLibrary: bad uri", e)
                nativeOnLibraryScanResult("")
                return@submitLogged
            }
            val root = DocumentFile.fromTreeUri(this, treeUri)
            if (root == null || !root.canRead()) {
                Log.w(TAG, "scanLibrary: tree not readable: $treeUri")
                nativeOnLibraryScanResult("")
                return@submitLogged
            }
            val out = JSONArray()
            // CRC32 cache keyed by document URI ("size:crc"), so a big library is
            // hashed once and re-scans are instant. Size guards against edits.
            val crcPrefs = getSharedPreferences("rom_crc", android.content.Context.MODE_PRIVATE)
            val crcEditor = crcPrefs.edit()
            data class Frame(val dir: DocumentFile, val relPath: String)
            val queue = ArrayDeque<Frame>()
            queue.add(Frame(root, ""))
            while (queue.isNotEmpty()) {
                val (dir, prefix) = queue.removeFirst()
                val children = try {
                    dir.listFiles()
                } catch (e: Exception) {
                    Log.w(TAG, "scanLibrary: listFiles failed at $prefix", e)
                    continue
                }
                for (child in children) {
                    val name = child.name ?: continue
                    val childRel = if (prefix.isEmpty()) name else "$prefix/$name"
                    if (child.isDirectory) {
                        queue.addLast(Frame(child, childRel))
                    } else if (child.isFile && RomScan.isRomFile(name)) {
                        val size = child.length()
                        val key = child.uri.toString()
                        var crc = 0L
                        val cached = crcPrefs.getString(key, null)
                        if (cached != null) {
                            val p = cached.split(":")
                            if (p.size == 2 && p[0].toLongOrNull() == size) {
                                crc = p[1].toLongOrNull() ?: 0L
                            }
                        }
                        if (crc == 0L) {
                            crc = computeCrc32(child.uri, name)
                            if (crc != 0L) crcEditor.putString(key, "$size:$crc")
                        }
                        val obj = JSONObject()
                        obj.put("uri", key)
                        obj.put("name", name)
                        obj.put("rel_path", childRel)
                        obj.put("size_bytes", size)
                        obj.put("crc32", crc)
                        out.put(obj)
                    }
                }
            }
            crcEditor.apply()
            Log.i(TAG, "scanLibrary: ${out.length()} entries")
            nativeOnLibraryScanResult(out.toString())
        }
    }

    /**
     * CRC32 of a ROM file, matching No-Intro's checksum of the raw ROM image.
     * For a `.zip`, hashes the first contained ROM entry (No-Intro CRCs are of
     * the uncompressed ROM, not the archive). Returns 0 on any error. The
     * zip/raw CRC logic lives in [RomScan.crcOfRomStream].
     */
    private fun computeCrc32(uri: Uri, name: String): Long {
        return try {
            contentResolver.openInputStream(uri)?.use { input ->
                RomScan.crcOfRomStream(input, name)
            } ?: 0L
        } catch (e: Exception) {
            Log.w(TAG, "computeCrc32 failed for $name", e)
            0L
        }
    }

    /**
     * Open the ROM at [romUriString], locate or create a writable
     * sibling `<rom-stem>.sav`, and hand the ROM bytes + sav fd to
     * native via [nativeOnRomLoaded]. On any failure, calls
     * [nativeOnRomLoadFailed].
     */
    @Suppress("unused")
    fun loadRomEntry(romUriString: String) {
        submitLogged("loadRomEntry", onFailure = { nativeOnRomLoadFailed() }) {
            val romUri = try {
                Uri.parse(romUriString)
            } catch (e: Exception) {
                Log.w(TAG, "loadRomEntry: bad uri", e)
                nativeOnRomLoadFailed()
                return@submitLogged
            }
            val romBytes = readAllBytes(romUri)
            if (romBytes == null) {
                Log.w(TAG, "loadRomEntry: failed to read ROM bytes")
                nativeOnRomLoadFailed()
                return@submitLogged
            }
            val romDoc = DocumentFile.fromSingleUri(this, romUri)
            val displayName = romDoc?.name ?: queryDisplayName(romUri) ?: "rom.gb"
            val savFd = openSiblingSavFd(romUri, displayName)
            Log.i(
                TAG,
                "loadRomEntry: ${romBytes.size} bytes ($displayName), sav fd=$savFd",
            )
            nativeOnRomLoaded(romBytes, displayName, savFd)
        }
    }

    private fun openSiblingSavFd(romUri: Uri, displayName: String): Int {
        val stem = RomScan.savStem(displayName)
        val savName = "$stem.sav"
        return try {
            // Derive the parent doc id by trimming the trailing path
            // component of the ROM's doc id, then build a tree-scoped
            // URI for that parent so we can call createFile/findFile.
            val docId = DocumentsContract.getDocumentId(romUri)
            val slash = docId.lastIndexOf('/')
            if (slash < 0) {
                Log.w(TAG, "openSiblingSavFd: no parent in docId=$docId")
                return -1
            }
            val parentDocId = docId.substring(0, slash)
            val parentTreeUri = persistedTreeUriContaining(romUri) ?: run {
                Log.w(TAG, "openSiblingSavFd: no persisted tree for $romUri")
                return -1
            }
            val parentTreeDocUri = DocumentsContract.buildDocumentUriUsingTree(
                parentTreeUri,
                parentDocId,
            )
            val parent = DocumentFile.fromTreeUri(this, parentTreeDocUri)
            if (parent == null) {
                Log.w(TAG, "openSiblingSavFd: parent DocumentFile null")
                return -1
            }
            val sav = parent.findFile(savName)
                ?: parent.createFile("application/octet-stream", savName)
            if (sav == null) {
                Log.w(TAG, "openSiblingSavFd: could not create $savName")
                return -1
            }
            val pfd = contentResolver.openFileDescriptor(sav.uri, "rw")
            if (pfd == null) {
                Log.w(TAG, "openSiblingSavFd: openFileDescriptor returned null")
                return -1
            }
            pfd.detachFd()
        } catch (e: Exception) {
            Log.w(TAG, "openSiblingSavFd: $e", e)
            -1
        }
    }

    private fun persistedTreeUriContaining(child: Uri): Uri? {
        for (perm in contentResolver.persistedUriPermissions) {
            if (perm.uri.authority == child.authority) {
                return perm.uri
            }
        }
        return contentResolver.persistedUriPermissions.firstOrNull()?.uri
    }

    private fun readAllBytes(uri: Uri): ByteArray? = try {
        contentResolver.openInputStream(uri)?.use { input ->
            val out = ByteArrayOutputStream(1 shl 16)
            val buf = ByteArray(16 * 1024)
            while (true) {
                val n = input.read(buf)
                if (n <= 0) break
                out.write(buf, 0, n)
            }
            out.toByteArray()
        } ?: run {
            Log.e(TAG, "openInputStream returned null for $uri")
            null
        }
    } catch (e: IOException) {
        Log.e(TAG, "Failed to read uri", e)
        null
    }

    private fun queryDisplayName(uri: Uri): String? = try {
        contentResolver.query(
            uri,
            arrayOf(OpenableColumns.DISPLAY_NAME),
            null, null, null,
        )?.use { cursor ->
            if (cursor.moveToFirst()) {
                val idx = cursor.getColumnIndex(OpenableColumns.DISPLAY_NAME)
                if (idx >= 0) cursor.getString(idx) else null
            } else null
        }
    } catch (e: Exception) {
        Log.w(TAG, "Failed to query display name", e)
        null
    }

    override fun onDestroy() {
        ioExecutor.shutdownNow()
        super.onDestroy()
    }

    companion object {
        private const val TAG = "rustyboi"

        // --------------------------------------------------------------------
        // Native callbacks. Implemented in rustyboi-platform/src/android.rs.
        // --------------------------------------------------------------------

        @JvmStatic
        private external fun nativeOnRomPicked(bytes: ByteArray, fileName: String)

        @JvmStatic
        private external fun nativeOnRomPickCancelled()

        @JvmStatic
        private external fun nativeOnTreePicked(treeUri: String)

        @JvmStatic
        private external fun nativeOnLibraryScanResult(entriesJson: String)

        @JvmStatic
        private external fun nativeOnRomLoaded(
            romBytes: ByteArray,
            displayName: String,
            savFd: Int,
        )

        @JvmStatic
        private external fun nativeOnRomLoadFailed()

        @JvmStatic
        private external fun nativeOnGamepadAxes(
            lx: Float,
            ly: Float,
            rx: Float,
            ry: Float,
            hatX: Float,
            hatY: Float,
            lt: Float,
            rt: Float,
        )
    }
}

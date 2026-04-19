package dev.mcswain.rustyboi

import android.content.Intent
import android.net.Uri
import android.view.InputDevice
import android.view.KeyEvent
import android.view.MotionEvent
import android.provider.DocumentsContract
import android.provider.OpenableColumns
import android.util.Log
import android.widget.Toast
import androidx.activity.result.ActivityResultLauncher
import androidx.activity.result.contract.ActivityResultContracts
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
class RustyboiActivity : GameActivity() {

    /** Single-threaded background reader so multiple picks serialize. */
    private val ioExecutor: ExecutorService = Executors.newSingleThreadExecutor()

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
            ioExecutor.submit {
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
     * analog motion. We hook the top-level dispatch so we can (a) LOG everything
     * to find where gamepad input actually flows, and (b) forward joystick axes
     * to Rust via JNI. dispatch* is the earliest activity-level hook (before view
     * dispatch), so it fires even if GameActivity consumes the event afterwards.
     */
    override fun dispatchGenericMotionEvent(event: MotionEvent): Boolean {
        val joystick = event.source and InputDevice.SOURCE_JOYSTICK ==
            InputDevice.SOURCE_JOYSTICK
        Log.i(
            TAG,
            "genericMotion src=0x%x action=%d joystick=%b x=%.2f y=%.2f z=%.2f rz=%.2f hatX=%.2f hatY=%.2f"
                .format(
                    event.source, event.action, joystick,
                    event.getAxisValue(MotionEvent.AXIS_X),
                    event.getAxisValue(MotionEvent.AXIS_Y),
                    event.getAxisValue(MotionEvent.AXIS_Z),
                    event.getAxisValue(MotionEvent.AXIS_RZ),
                    event.getAxisValue(MotionEvent.AXIS_HAT_X),
                    event.getAxisValue(MotionEvent.AXIS_HAT_Y),
                ),
        )
        if (joystick) {
            nativeOnGamepadAxes(
                event.getAxisValue(MotionEvent.AXIS_X),
                event.getAxisValue(MotionEvent.AXIS_Y),
                event.getAxisValue(MotionEvent.AXIS_Z),
                event.getAxisValue(MotionEvent.AXIS_RZ),
                event.getAxisValue(MotionEvent.AXIS_HAT_X),
                event.getAxisValue(MotionEvent.AXIS_HAT_Y),
            )
        }
        return super.dispatchGenericMotionEvent(event)
    }

    override fun dispatchKeyEvent(event: KeyEvent): Boolean {
        Log.i(
            TAG,
            "keyEvent code=%d action=%d src=0x%x".format(event.keyCode, event.action, event.source),
        )
        return super.dispatchKeyEvent(event)
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
        ioExecutor.submit {
            val treeUri = try {
                Uri.parse(treeUriString)
            } catch (e: Exception) {
                Log.w(TAG, "scanLibrary: bad uri", e)
                nativeOnLibraryScanResult("")
                return@submit
            }
            val root = DocumentFile.fromTreeUri(this, treeUri)
            if (root == null || !root.canRead()) {
                Log.w(TAG, "scanLibrary: tree not readable: $treeUri")
                nativeOnLibraryScanResult("")
                return@submit
            }
            val out = JSONArray()
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
                    } else if (child.isFile && isRomFile(name)) {
                        val obj = JSONObject()
                        obj.put("uri", child.uri.toString())
                        obj.put("name", name)
                        obj.put("rel_path", childRel)
                        obj.put("size_bytes", child.length())
                        out.put(obj)
                    }
                }
            }
            Log.i(TAG, "scanLibrary: ${out.length()} entries")
            nativeOnLibraryScanResult(out.toString())
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
        ioExecutor.submit {
            val romUri = try {
                Uri.parse(romUriString)
            } catch (e: Exception) {
                Log.w(TAG, "loadRomEntry: bad uri", e)
                nativeOnRomLoadFailed()
                return@submit
            }
            val romBytes = readAllBytes(romUri)
            if (romBytes == null) {
                Log.w(TAG, "loadRomEntry: failed to read ROM bytes")
                nativeOnRomLoadFailed()
                return@submit
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
        val stem = savStem(displayName)
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

    private fun savStem(displayName: String): String {
        // Strip the outermost extension. For `Pokemon Crystal.zip` we
        // want `Pokemon Crystal.sav`; for `Pokemon Crystal.gbc` we
        // also want `Pokemon Crystal.sav`.
        val dot = displayName.lastIndexOf('.')
        return if (dot <= 0) displayName else displayName.substring(0, dot)
    }

    private fun isRomFile(name: String): Boolean {
        val lower = name.lowercase()
        return lower.endsWith(".gb") ||
                lower.endsWith(".gbc") ||
                lower.endsWith(".sgb") ||
                lower.endsWith(".zip")
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
        )
    }
}

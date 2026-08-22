package studio.auru.pm;

import java.nio.charset.StandardCharsets;
import java.nio.file.Path;
import java.util.Optional;

/**
 * The compute half: what the pure Java client deliberately lacks.
 *
 * <p>{@link AuruClient} on its own speaks the protocol, derives commit ids, and verifies hashes —
 * everything needed to browse history and publish. It cannot read a DAW project file, because that
 * is twenty-odd thousand lines of format work and a second implementation would drift from the
 * first. This class links that work as a native library instead of reimplementing it, so a phone
 * can answer "what changed between these two versions" with the same code the desktop runs.
 *
 * <p><strong>Optional.</strong> Nothing else in this library references this class, so an
 * application that never calls it never loads it and never needs the native library — the same
 * arrangement that keeps {@link JdkHttpTransport} out of an Android build.
 *
 * <p>Load the library once at startup, before any other method here:
 *
 * <pre>{@code
 * Kernel.loadFromSystemLibraryPath();   // Android: jniLibs/<abi>/libauru_pm_ffi.so
 * // or
 * Kernel.load(Path.of("/path/to/libauru_pm_ffi.dylib"));
 * }</pre>
 *
 * <p>Every method is thread-safe; the kernel holds no state.
 */
public final class Kernel {

    private Kernel() {}

    /**
     * Load the native library from the system library path.
     *
     * <p>On Android this finds {@code jniLibs/<abi>/libauru_pm_ffi.so} automatically.
     *
     * @throws UnsatisfiedLinkError if no library for this platform is present
     */
    public static void loadFromSystemLibraryPath() {
        System.loadLibrary("auru_pm_ffi");
    }

    /** Load the native library from an explicit path. */
    public static void load(Path library) {
        System.load(library.toAbsolutePath().toString());
    }

    /**
     * The wire protocol version this kernel was built for.
     *
     * <p>Worth asserting against {@link Protocol#VERSION} at startup: a native artifact and a jar
     * can drift out of step in a way neither notices otherwise.
     */
    public static native String protocolVersion();

    /** Whether this kernel and the pure client agree on the protocol version. */
    public static boolean matchesClientProtocol() {
        return Protocol.VERSION.equals(protocolVersion());
    }

    // ── Commit identity ──────────────────────────────────────────────────────

    /**
     * Derive a commit's id from its content, using the kernel's canonical rule.
     *
     * <p>The pure client derives the same id from the same commit — these are two implementations
     * of one specification, checked against the same vectors. This exists so a caller holding a
     * kernel can avoid the Java path as well, not because the answers differ.
     */
    public static native String commitId(String commitJson);

    /** The exact bytes a commit's id is the BLAKE3 of. */
    public static native byte[] commitCanonicalEncoding(String commitJson);

    /** BLAKE3 of {@code bytes}, as {@code blake3:<64 hex>}. */
    public static native String contentHash(byte[] bytes);

    private static native int verifyBlob(byte[] bytes, String expected);

    /**
     * Whether {@code bytes} hash to {@code expected}.
     *
     * @throws AuruKernelException if {@code expected} is not a content hash — a case a boolean
     *     could not carry, and one a caller must not read as "no"
     */
    public static boolean verify(byte[] bytes, ContentHash expected) {
        return verifyBlob(bytes, expected.toString()) == 1;
    }

    // ── Reading a project ────────────────────────────────────────────────────

    /**
     * Identify a project file from its name and leading bytes.
     *
     * <p>Returns the wire value — {@code ableton-live-set}, {@code fl-studio}, {@code dawproject},
     * {@code auru}, {@code bitwig-project}.
     */
    public static native String detectFormat(String fileName, byte[] source);

    /**
     * Normalize a project file into canonical snapshot bytes.
     *
     * <p>This is the call a phone cannot make without the kernel.
     */
    public static native byte[] snapshotFromSource(String format, byte[] source);

    /** Rebuild the original project file from canonical snapshot bytes. */
    public static native byte[] restoreFromSnapshot(byte[] canonical);

    private static native String projectInfoFromSnapshot(byte[] snapshot);

    /**
     * The few-kilobyte summary of what a snapshot is.
     *
     * <p>Empty when the snapshot is of a format this build cannot summarize; the caller then falls
     * back to reading the snapshot itself. That is a different outcome from a failure, which
     * throws.
     */
    public static Optional<ProjectInfo> projectInfo(byte[] snapshot) {
        String json = projectInfoFromSnapshot(snapshot);
        return json == null
                ? Optional.empty()
                : Optional.of(ProjectInfo.parse(json.getBytes(StandardCharsets.UTF_8)));
    }

    // ── Comparing versions ───────────────────────────────────────────────────

    private static native String diffSnapshots(byte[] before, byte[] after);

    /**
     * Per-channel structured diff between two canonical snapshots.
     *
     * <p>The reason this class exists: a history screen that shows what changed rather than only
     * that something did.
     */
    public static Json diff(byte[] before, byte[] after) {
        return Json.parse(diffSnapshots(before, after));
    }

    private static native String summarizeSnapshots(byte[] before, byte[] after);

    /** One line per change, between two canonical snapshots. */
    public static java.util.List<String> summarize(byte[] before, byte[] after) {
        return Json.parse(summarizeSnapshots(before, after)).elements().stream()
                .map(value -> ((Json.Str) value).value())
                .toList();
    }

    private static native String mergeSnapshots(byte[] ancestor, byte[] local, byte[] remote);

    /**
     * Three-way merge of canonical snapshots.
     *
     * <p>The result is a tagged union: {@code {"outcome":"clean","merged":…}} or
     * {@code {"outcome":"conflict","base":…,"conflicts":[…]}}.
     */
    public static Json merge(byte[] ancestor, byte[] local, byte[] remote) {
        return Json.parse(mergeSnapshots(ancestor, local, remote));
    }
}

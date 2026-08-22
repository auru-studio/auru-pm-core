package studio.auru.pm;

/** A failure inside the native compute kernel. */
public final class AuruKernelException extends RuntimeException {

    private static final long serialVersionUID = 1L;

    /** Called from JNI. */
    AuruKernelException(String message) {
        super(message);
    }
}

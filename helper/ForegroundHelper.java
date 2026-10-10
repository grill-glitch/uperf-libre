import java.lang.reflect.*;
import java.io.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.util.*;

/**
 * uperf foreground helper — the top app, event-driven, without `dumpsys`.
 *
 * Why this exists: `cpp/dfps/source/modules/topapp_monitor.cpp` learns the top app from
 * `GetTopAppNameDumpsys()` — a spawned process per lookup — and therefore gates the lookup
 * on the top-app cgroup's pid *count* moving by more than `TOP_TASK_NR_DIFF_MIN` (10). A
 * switch between two apps whose process counts differ by fewer than ten pids is invisible,
 * which is a documented defect of the scene/hint path. The gate is a cost workaround, not a
 * property of the problem: the honest fix is a source that is notified instead of polled.
 *
 * What it is: a bare `app_process` (root), no Android app context, so the hidden-API filter
 * does not apply and reflection can reach the framework. It registers an
 * `ITaskStackListener` — the callback fires on the switch itself — and on each callback
 * reads `getFocusedRootTaskInfo()`. On the device this was verified to exist alongside
 * `getTasks(int,boolean,boolean,int)`; both are tried, preferring the cheap one.
 *
 * The output is a file the daemon polls, one line, written atomically:
 *
 *     <package> <uptime_ms>
 *
 * or `- <uptime_ms>` when the top task cannot be resolved. The timestamp is uptime, not
 * wall clock, so a freshness check cannot be fooled by a clock jump. A line is written on
 * every poll as well as on every event: a file that only changed on switches would be
 * indistinguishable from a helper that died.
 *
 * Usage: CLASSPATH=<jar> app_process /system/bin ForegroundHelper <out-file> [poll_ms]
 */
public class ForegroundHelper {
    private static Method UPTIME;

    /**
     * The listener, as a named class and not a lambda: `d8` cannot desugar a lambda whose
     * interface it cannot resolve (`java.lang.reflect.InvocationHandler` was reported
     * missing without `--lib android.jar`), and the resulting dex dies at load with no
     * output at all — which is exactly how the first build of this failed.
     */
    static final class StackListener implements InvocationHandler {
        final Object svc;
        final Class<?> iface;
        final File out;
        final String[] last = {null};

        StackListener(Object svc, Class<?> iface, File out) {
            this.svc = svc;
            this.iface = iface;
            this.out = out;
        }

        public Object invoke(Object proxy, Method method, Object[] args) throws Throwable {
            if (method.getName().startsWith("on")) {
                write(out, readTop(svc, iface), last);
            }
            Class<?> r = method.getReturnType();
            if (r == void.class) return null;
            if (r == boolean.class) return Boolean.FALSE;
            if (r == int.class) return Integer.valueOf(0);
            if (r == long.class) return Long.valueOf(0L);
            if (r == float.class) return Float.valueOf(0f);
            if (r == double.class) return Double.valueOf(0d);
            return null;
        }
    }

    static long uptimeMs() {
        try {
            if (UPTIME == null) UPTIME = Class.forName("android.os.SystemClock").getMethod("uptimeMillis");
            return (Long) UPTIME.invoke(null);
        } catch (Throwable t) {
            return System.nanoTime() / 1000000L;  // monotonic; only compared against itself
        }
    }

    /** The top task's package, or null. Prefers the single focused-task read. */
    static String readTop(Object svc, Class<?> iface) {
        // `getFocusedRootTaskInfo()` — one call, exactly the question being asked.
        try {
            Method m = iface.getMethod("getFocusedRootTaskInfo");
            Object info = m.invoke(svc);
            String p = packageOf(info);
            if (p != null) return p;
        } catch (Throwable ignored) {
        }
        // Fallback for a build where the above is absent: getTasks(max, visibleOnly,
        // keepIntentExtra, displayId) — the 5-arg form seen on this device.
        try {
            Method m = iface.getMethod("getTasks", int.class, boolean.class, boolean.class, int.class);
            Object res = m.invoke(svc, 1, false, false, 0);
            if (res instanceof List) {
                for (Object t : (List<?>) res) {
                    String p = packageOf(t);
                    if (p != null) return p;
                }
            }
        } catch (Throwable ignored) {
        }
        return null;
    }

    /**
     * Package of a RootTaskInfo / RunningTaskInfo / TaskInfo.
     *
     * On this build these carry `ComponentName topActivity / baseActivity / origActivity /
     * realActivity` as **public fields** — verified on-device with FgProbe2 (`getField` finds
     * them, `getMethod` throws NoSuchMethodException). The original getter-only lookup
     * therefore returned null for every task, which is exactly why the first device run
     * registered fine yet wrote `-`. Read the fields first, then fall back to getters for a
     * build that exposes them.
     */
    static String packageOf(Object info) {
        if (info == null) return null;
        for (String name : new String[]{"topActivity", "baseActivity", "origActivity", "realActivity"}) {
            Object comp = null;
            try {
                comp = info.getClass().getField(name).get(info);
            } catch (Throwable ignored) {
            }
            if (comp == null) {
                try {
                    comp = info.getClass().getMethod(name).invoke(info);
                } catch (Throwable ignored) {
                }
            }
            String p = pkgOfComponent(comp);
            if (p != null) return p;
        }
        return null;
    }

    /** Package of a ComponentName (or any CharSequence naming one, e.g. `pkg/.Act`). */
    static String pkgOfComponent(Object comp) {
        if (comp == null) return null;
        try {
            Object p = comp.getClass().getMethod("getPackageName").invoke(comp);
            if (p instanceof String && !((String) p).isEmpty()) return (String) p;
        } catch (Throwable ignored) {
        }
        if (comp instanceof CharSequence) {
            String s = comp.toString();
            int slash = s.indexOf('/');
            if (slash > 0) return s.substring(0, slash);
        }
        return null;
    }

    /** Write atomically: a reader must never see half a line. */
    static void write(File out, String pkg, String[] last) {
        String line = (pkg == null ? "-" : pkg) + " " + uptimeMs() + "\n";
        boolean changed = last[0] == null || !last[0].equals(line.substring(0, line.indexOf(' ')));
        last[0] = line.substring(0, line.indexOf(' '));
        File tmp = new File(out.getAbsolutePath() + ".new");
        try {
            try (Writer w = new OutputStreamWriter(new FileOutputStream(tmp), StandardCharsets.UTF_8)) {
                w.write(line);
            }
            if (!tmp.renameTo(out)) {
                Files.move(tmp.toPath(), out.toPath(), StandardCopyOption.REPLACE_EXISTING);
            }
            if (changed) err("topapp " + last[0] + " @ " + line.trim());
        } catch (Throwable t) {
            err("write failed: " + t);
        }
    }

    /** stderr, not stdout: stdout to a pipe is buffered and a killed helper loses it —
     *  which is how the first runs of this looked like they had produced no output at all. */
    static void err(String s) {
        System.err.println(s);
        System.err.flush();
    }

    public static void main(String[] args) throws Exception {
        err("start classpath=" + System.getProperty("java.class.path"));
        File out = new File(args.length > 0 ? args[0] : "/data/local/tmp/foreground.txt");
        long poll = args.length > 1 ? Long.parseLong(args[1]) : 5000L;

        Object svc = Class.forName("android.app.ActivityTaskManager").getMethod("getService").invoke(null);
        if (svc == null) {
            err("no ActivityTaskManager service");
            System.exit(2);
        }
        Class<?> iface = null;
        for (Class<?> i : svc.getClass().getInterfaces()) {
            if (i.getName().endsWith("IActivityTaskManager")) iface = i;
        }
        if (iface == null) {
            err("no IActivityTaskManager interface");
            System.exit(2);
        }

        final String[] last = {null};
        final Object fsvc = svc;
        final Class<?> fiface = iface;
        final File fout = out;

        // The listener interface is hidden, so the proxy is built against it by name and
        // answers every AIDL method (defaults for the return type, no-op for void).
        Class<?> listenerIface = Class.forName("android.app.ITaskStackListener");
        Object listener = Proxy.newProxyInstance(ForegroundHelper.class.getClassLoader(),
                new Class<?>[]{listenerIface}, new StackListener(fsvc, fiface, fout));

        err("service=" + svc.getClass().getName() + " iface=" + iface.getName());
        err("listener iface=" + listenerIface.getName());
        err("registering");
        iface.getMethod("registerTaskStackListener", listenerIface).invoke(svc, listener);
        err("registered");
        write(out, readTop(svc, iface), last);
        err("first write done: " + (readTop(svc, iface)));
        while (true) {
            Thread.sleep(poll);
            write(out, readTop(svc, iface), last);
        }
    }
}

import java.lang.reflect.*;
import java.util.List;

/** Throwaway probe: can a root app_process read the top task, and register a stack
 *  listener? Reflection only (the hidden API filter does not apply to a bare
 *  app_process), and it prints the exact signature it finds so the real helper can be
 *  written against facts instead of guesses. */
public class FgProbe {
    static void dumpMethods(Object o, String name) {
        Class<?> c = o.getClass();
        System.out.println("  " + name + " -> " + c.getName());
        for (Method m : c.getMethods()) {
            if (m.getName().equals("getTasks") || m.getName().equals("registerTaskStackListener")
                || m.getName().equals("getFocusedRootTaskInfo") || m.getName().startsWith("register")) {
                StringBuilder sb = new StringBuilder();
                for (Class<?> p : m.getParameterTypes()) sb.append(p.getSimpleName()).append(",");
                System.out.println("    " + m.getName() + "(" + sb + ")");
            }
        }
    }

    public static void main(String[] args) throws Exception {
        Object svc = null;
        for (String cn : new String[]{"android.app.ActivityTaskManager", "android.app.ActivityManager"}) {
            try {
                Class<?> c = Class.forName(cn);
                try {
                    svc = c.getMethod("getService").invoke(null);
                } catch (Throwable t) {
                    Field f = c.getField("IActivityTaskManagerSingleton");
                    System.out.println("singleton=" + f);
                }
                System.out.println("getService via " + cn + " -> " + (svc == null ? "null" : svc.getClass().getName()));
                if (svc != null) break;
            } catch (Throwable t) {
                System.out.println("  " + cn + ": " + t);
            }
        }
        if (svc == null) { System.out.println("NO_SERVICE"); return; }
        dumpMethods(svc, "service");

        Class<?> iface = null;
        for (Class<?> i : svc.getClass().getInterfaces()) {
            System.out.println("  iface: " + i.getName());
            if (i.getName().endsWith("IActivityTaskManager")) iface = i;
        }
        if (iface != null) {
            Method gt = null;
            try { gt = iface.getMethod("getTasks", int.class); } catch (Throwable t) {}
            System.out.println("getTasks(int) = " + gt);
            if (gt != null) {
                try {
                    Object res = gt.invoke(svc, 3);
                    List<?> list = (List<?>) res;
                    System.out.println("tasks=" + list.size());
                    for (Object t : list) {
                        Class<?> tc = t.getClass();
                        Object base = null;
                        for (Method m : tc.getMethods()) {
                            if (m.getName().equals("getBaseActivity") || m.getName().equals("getTopActivity")) {
                                try { base = m.invoke(t); } catch (Throwable x) {}
                                if (base != null) { System.out.println("  top via " + m.getName()); break; }
                            }
                        }
                        String pkg = "?";
                        if (base != null) {
                            try { pkg = (String) base.getClass().getMethod("getPackageName").invoke(base); } catch (Throwable x) {}
                        }
                        System.out.println("  task " + tc.getSimpleName() + " pkg=" + pkg + " comp=" + base);
                    }
                } catch (Throwable t) {
                    System.out.println("getTasks call failed: " + t);
                }
            }
        }
    }
}

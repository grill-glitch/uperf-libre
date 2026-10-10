import java.lang.reflect.*;
import java.util.List;

/** Probe 2: what does getFocusedRootTaskInfo() actually return, and where is the package?
 *  Fields vs getters is the open question. */
public class FgProbe2 {
    static void dump(Object o, String label) {
        if (o == null) { System.out.println(label + " = null"); return; }
        Class<?> c = o.getClass();
        System.out.println(label + " class=" + c.getName());
        for (Field f : c.getFields()) {
            try {
                Object v = f.get(o);
                String s = String.valueOf(v);
                if (s.length() > 140) s = s.substring(0, 140) + "...";
                System.out.println("  field " + f.getType().getSimpleName() + " " + f.getName() + " = " + s);
            } catch (Throwable t) { System.out.println("  field " + f.getName() + " ERR " + t); }
        }
    }

    public static void main(String[] a) throws Exception {
        Object svc = Class.forName("android.app.ActivityTaskManager").getMethod("getService").invoke(null);
        Class<?> iface = null;
        for (Class<?> i : svc.getClass().getInterfaces())
            if (i.getName().endsWith("IActivityTaskManager")) iface = i;
        System.out.println("svc=" + svc.getClass().getName() + " iface=" + iface.getName());
        try {
            Object info = iface.getMethod("getFocusedRootTaskInfo").invoke(svc);
            dump(info, "getFocusedRootTaskInfo()");
        } catch (Throwable t) { System.out.println("getFocusedRootTaskInfo failed: " + t); }
        try {
            Object res = iface.getMethod("getTasks", int.class, boolean.class, boolean.class, int.class)
                    .invoke(svc, 5, false, false, 0);
            List<?> l = (List<?>) res;
            System.out.println("getTasks(5,false,false,0) size=" + (l == null ? -1 : l.size()));
            if (l != null) for (Object t : l) dump(t, " task");
        } catch (Throwable t) { System.out.println("getTasks failed: " + t); }
    }
}

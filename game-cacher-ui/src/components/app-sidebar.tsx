import { useState } from "react";
import { NavLink, useNavigate } from "react-router";
import { Activity, Gamepad2, History, LayoutDashboard, LogOut, Settings } from "lucide-react";
import { logout } from "@/api/client";
import { useAuth } from "@/context/auth";
import { useToast } from "@/context/toast";
import { errorMessage } from "@/api/client";
import { cn } from "@/lib/utils";

interface NavItem {
  title: string;
  href: string;
  icon: React.ComponentType<{ className?: string }>;
  end?: boolean;
}

const mainNavItems: NavItem[] = [
  { title: "Dashboard", href: "/", icon: LayoutDashboard, end: true },
  { title: "Platforms", href: "/platforms", icon: Gamepad2 },
  { title: "Runs", href: "/runs", icon: History },
];

const bottomNavItems: NavItem[] = [{ title: "Settings", href: "/settings", icon: Settings }];

const itemClass = "flex items-center gap-2 py-2 px-5 text-sm transition-colors cursor-pointer border-l-[3px] outline-none focus-visible:bg-bg-raised focus-visible:text-text-primary w-full text-left";
const idleClass = "text-text-secondary bg-transparent border-transparent hover:text-text-primary hover:bg-bg-raised";

function SidebarNavItem({ item }: { item: NavItem }) {
  const Icon = item.icon;
  return (
    <NavLink
      to={item.href}
      end={item.end}
      className={({ isActive }) =>
        cn(itemClass, isActive ? "text-text-primary bg-bg-raised border-accent" : idleClass)
      }
    >
      <Icon className="w-4 h-4 shrink-0" />
      <span className="flex-1">{item.title}</span>
    </NavLink>
  );
}

export function AppSidebar() {
  const { state, refresh } = useAuth();
  const navigate = useNavigate();
  const toast = useToast();
  const [busy, setBusy] = useState(false);

  const onLogout = async () => {
    setBusy(true);
    try {
      await logout();
      await refresh();
      navigate("/login", { replace: true });
    } catch (e) {
      toast.push({ variant: "error", title: "Sign out failed", message: errorMessage(e) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <aside className="flex flex-col w-[200px] shrink-0 h-screen bg-bg-surface border-r border-border">
      {/* Logo */}
      <div className="px-5 pt-5 pb-4">
        <div className="text-xl font-bold text-accent leading-none">GC</div>
        <div className="text-xs text-text-secondary uppercase mt-1 tracking-[3px]">game cacher</div>
      </div>

      <div className="mx-5 border-t border-border" />

      <nav aria-label="Main" className="flex-1 overflow-y-auto py-3">
        {mainNavItems.map((item) => (
          <SidebarNavItem key={item.href} item={item} />
        ))}
      </nav>

      <div className="mx-5 border-t border-border" />
      <nav aria-label="System" className="py-3">
        {bottomNavItems.map((item) => (
          <SidebarNavItem key={item.href} item={item} />
        ))}
        <a href="/metrics" target="_blank" rel="noreferrer" className={cn(itemClass, idleClass)}>
          <Activity className="w-4 h-4 shrink-0" />
          <span className="flex-1">Metrics</span>
        </a>
        {state.auth_enabled && (
          <button type="button" onClick={onLogout} disabled={busy} className={cn(itemClass, idleClass, "disabled:opacity-50")}>
            <LogOut className="w-4 h-4 shrink-0" />
            <span className="flex-1">Sign out</span>
          </button>
        )}
      </nav>
    </aside>
  );
}

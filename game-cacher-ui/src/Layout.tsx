import { useEffect, useRef } from "react";
import { Navigate, Outlet, useLocation } from "react-router";
import { AppSidebar } from "@/components/app-sidebar";
import { useAuth } from "@/context/auth";

export default function Layout() {
  const { pathname } = useLocation();
  const { state } = useAuth();
  const mainRef = useRef<HTMLElement>(null);

  // <main> is the scroll container; start each page at the top.
  useEffect(() => {
    mainRef.current?.scrollTo({ top: 0 });
  }, [pathname]);

  if (state.auth_enabled && !state.authenticated) return <Navigate to="/login" replace />;

  return (
    <div className="flex h-screen bg-bg-base">
      <AppSidebar />
      <main ref={mainRef} className="flex-1 min-w-0 overflow-y-auto p-6 md:p-8">
        <div className="max-w-[1400px] mx-auto">
          <Outlet />
        </div>
      </main>
    </div>
  );
}

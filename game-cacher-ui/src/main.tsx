import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { createBrowserRouter } from "react-router";
import { RouterProvider } from "react-router/dom";
import "./index.css";

import Layout from "./Layout.tsx";
import { AuthProvider } from "./context/auth.tsx";
import { ToastProvider } from "./context/toast.tsx";
import { TooltipProvider } from "./components/ui/tooltip.tsx";
import Dashboard from "./pages/Dashboard.tsx";
import Login from "./pages/Login.tsx";
import Platforms from "./pages/Platforms.tsx";
import PlatformDetail from "./pages/PlatformDetail.tsx";
import SettingsPage from "./pages/Settings.tsx";
import NotFound from "./pages/NotFound.tsx";

const router = createBrowserRouter([
  { path: "/login", Component: Login },
  {
    Component: Layout,
    children: [
      { index: true, Component: Dashboard },
      { path: "/platforms", Component: Platforms },
      { path: "/platforms/:id", Component: PlatformDetail },
      { path: "/settings", Component: SettingsPage },
      { path: "*", Component: NotFound },
    ],
  },
]);

createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <TooltipProvider>
      <ToastProvider>
        <AuthProvider>
          <RouterProvider router={router} />
        </AuthProvider>
      </ToastProvider>
    </TooltipProvider>
  </StrictMode>,
);

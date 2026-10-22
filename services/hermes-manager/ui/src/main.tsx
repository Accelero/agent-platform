import { setNonce } from "get-nonce";
setNonce(
  document.querySelector<HTMLMetaElement>('meta[name="manager-style-nonce"]')
    ?.content ?? "",
);
import { StrictMode } from "react";
import { createRoot } from "react-dom/client";
import { QueryClientProvider } from "@tanstack/react-query";
import { RouterProvider } from "@tanstack/react-router";
import { queryClient } from "./app/state";
import { router } from "./app/router";
import "./style.css";
createRoot(document.getElementById("root")!).render(
  <StrictMode>
    <QueryClientProvider client={queryClient}>
      <RouterProvider router={router} />
    </QueryClientProvider>
  </StrictMode>,
);

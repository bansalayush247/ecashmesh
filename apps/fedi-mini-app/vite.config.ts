import react from "@vitejs/plugin-react";
import { defineConfig, loadEnv } from "vite";

// In development the app reaches EcashMesh through this server's /api proxy,
// so the EcashMesh server needs no CORS entry for it. A deployed mini app
// sets VITE_ECASHMESH_API_URL to a server that allows its origin.
export default defineConfig(({ mode }) => {
  const env = loadEnv(mode, ".", "");
  return {
    plugins: [react()],
    // The client is linked as TypeScript source with file:; resolve its zod
    // from this app's node_modules.
    resolve: { preserveSymlinks: true },
    server: {
      port: 8082,
      strictPort: true,
      proxy: {
        "/api": {
          target: env.ECASHMESH_DEV_API ?? "http://127.0.0.1:5000",
          rewrite: (path) => path.replace(/^\/api/, ""),
        },
      },
    },
  };
});

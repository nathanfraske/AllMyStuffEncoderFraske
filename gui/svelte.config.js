import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

export default {
  // Svelte 5 with TypeScript in <script lang="ts">.
  preprocess: vitePreprocess(),
  compilerOptions: {
    runes: true,
  },
};

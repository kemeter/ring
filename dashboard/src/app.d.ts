// See https://kit.svelte.dev/docs/types#app
declare global {
  namespace App {
    // interface Error {}
    // interface Locals {}
    // interface PageData {}
    // interface Platform {}
  }

  /** Injected by Vite `define` from the workspace Cargo.toml version. */
  const __RING_VERSION__: string;
}

export {};

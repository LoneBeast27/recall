<script lang="ts">
  import { onMount } from 'svelte';
  import { page } from '$app/stores';
  import { MODULES } from '$lib/modules';
  import { startDaemonFeed, connected, engine } from '$lib/daemon';

  let { children } = $props();

  onMount(() => {
    startDaemonFeed();
  });
</script>

<div class="shell">
  <aside class="nav">
    <div class="brand">Recall<span>Hub</span></div>
    {#each MODULES as m}
      <a class="navitem" class:active={$page.url.pathname === m.route} href={m.route}>
        <span class="icon">{m.icon}</span>{m.title}
      </a>
    {/each}
    <div class="spacer"></div>
    <div class="conn" class:ok={$connected}>
      <span class="dot"></span>
      {$connected ? 'daemon connected' : 'daemon offline'}
    </div>
  </aside>
  <main class="content">
    {@render children()}
  </main>
</div>
<footer class="statusbar">
  {#if $engine}
    <span>{$engine.source}</span>
    <span>{$engine.width}×{$engine.height} @ {$engine.fps}fps</span>
    <span>{$engine.display.toUpperCase()} · {$engine.mode}</span>
    <span>encoder: {$engine.encoder}</span>
  {:else}
    <span>waiting for engine…</span>
  {/if}
</footer>

<style>
  :global(html, body) {
    margin: 0;
    height: 100%;
    background: #0e1116;
    color: #dbe2ea;
    font-family: 'Segoe UI', system-ui, sans-serif;
    user-select: none;
  }
  .shell {
    display: flex;
    height: calc(100vh - 28px);
  }
  .nav {
    width: 172px;
    background: #12161d;
    border-right: 1px solid #1f2630;
    display: flex;
    flex-direction: column;
    padding: 14px 10px;
    gap: 4px;
  }
  .brand {
    font-weight: 700;
    font-size: 17px;
    letter-spacing: 0.5px;
    padding: 2px 8px 14px;
  }
  .brand span {
    color: #4f9cff;
  }
  .navitem {
    display: flex;
    align-items: center;
    gap: 10px;
    padding: 9px 10px;
    border-radius: 8px;
    color: #9aa7b5;
    text-decoration: none;
    font-size: 14px;
  }
  .navitem:hover {
    background: #1a212b;
    color: #dbe2ea;
  }
  .navitem.active {
    background: #1d2836;
    color: #eaf2fb;
  }
  .icon {
    width: 20px;
    text-align: center;
  }
  .spacer {
    flex: 1;
  }
  .conn {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: 12px;
    color: #8b6a6a;
    padding: 6px 8px;
  }
  .conn.ok {
    color: #7fb58a;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
    background: currentColor;
  }
  .content {
    flex: 1;
    overflow-y: auto;
    padding: 22px 26px;
  }
  .statusbar {
    height: 28px;
    display: flex;
    align-items: center;
    gap: 22px;
    padding: 0 16px;
    background: #12161d;
    border-top: 1px solid #1f2630;
    font-size: 12px;
    color: #9aa7b5;
  }
</style>

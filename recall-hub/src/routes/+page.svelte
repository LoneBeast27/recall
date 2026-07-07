<script lang="ts">
  import { onMount } from 'svelte';
  import { convertFileSrc } from '@tauri-apps/api/core';
  import {
    engine,
    lastSaved,
    saveError,
    saveClip,
    listClips,
    showInFolder,
    type ClipEntry,
  } from '$lib/daemon';

  let clips: ClipEntry[] = $state([]);
  let toastUntil = $state(0);

  const refresh = async () => {
    try {
      clips = await listClips(8);
    } catch {
      clips = [];
    }
  };

  onMount(refresh);

  // New save -> toast + refresh the strip.
  $effect(() => {
    if ($lastSaved) {
      toastUntil = $lastSaved.at + 6000;
      refresh();
    }
  });

  const bufferPct = $derived(
    $engine ? Math.min(100, ($engine.buffer_secs / 60) * 100) : 0
  );
  const mb = (b: number) => (b / 1024 / 1024).toFixed(1);
  const baseName = (p: string) => p.split('\\').pop() ?? p;
  const timeOf = (m: number) => new Date(m * 1000).toLocaleTimeString();
</script>

<h1>Clips</h1>

<section class="panel">
  <div class="row">
    <div class="gauge">
      <div class="gauge-label">
        replay buffer
        <b>{$engine ? $engine.buffer_secs.toFixed(0) : '–'}s</b> / 60s
      </div>
      <div class="bar"><div class="fill" style="width:{bufferPct}%"></div></div>
      <div class="gauge-sub">
        {#if $engine}
          {$engine.encoder} · capturing “{$engine.source}”
        {:else}
          engine offline
        {/if}
      </div>
    </div>
    <div class="actions">
      <button class="save big" onclick={() => saveClip()}>Save 60s</button>
      <button class="save" onclick={() => saveClip(30)}>30s</button>
      <button class="save" onclick={() => saveClip(15)}>15s</button>
    </div>
  </div>

  {#if $lastSaved && Date.now() < toastUntil}
    <div class="toast ok">
      Saved ✓ {$lastSaved.seconds.toFixed(1)}s · {mb($lastSaved.bytes)} MB ·
      {$lastSaved.elapsed_ms}ms — {baseName($lastSaved.path)}
    </div>
  {/if}
  {#if $saveError}
    <div class="toast err">Save failed: {$saveError}</div>
  {/if}
</section>

<h2>Recent</h2>
<section class="grid">
  {#each clips as c (c.path)}
    <button class="clip" onclick={() => showInFolder(c.path)} title="Reveal in Explorer">
      {#if c.thumb}
        <img src={convertFileSrc(c.thumb)} alt={c.name} />
      {:else}
        <div class="nothumb">no thumb</div>
      {/if}
      <div class="meta">
        <div class="name">{c.name}</div>
        <div class="sub">{timeOf(c.mtime)} · {mb(c.bytes)} MB</div>
      </div>
    </button>
  {:else}
    <div class="empty">No clips yet — hit Save (or Ctrl+Alt+S anywhere).</div>
  {/each}
</section>

<style>
  h1 {
    font-size: 20px;
    margin: 0 0 14px;
  }
  h2 {
    font-size: 14px;
    color: #9aa7b5;
    margin: 22px 0 10px;
    text-transform: uppercase;
    letter-spacing: 1px;
  }
  .panel {
    background: #161b22;
    border: 1px solid #1f2630;
    border-radius: 12px;
    padding: 18px;
  }
  .row {
    display: flex;
    gap: 24px;
    align-items: center;
  }
  .gauge {
    flex: 1;
  }
  .gauge-label {
    font-size: 13px;
    color: #9aa7b5;
    margin-bottom: 8px;
  }
  .gauge-label b {
    color: #eaf2fb;
    font-size: 15px;
  }
  .bar {
    height: 10px;
    border-radius: 5px;
    background: #0e1116;
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: linear-gradient(90deg, #2f6fd8, #4f9cff);
    transition: width 0.5s ease;
  }
  .gauge-sub {
    margin-top: 8px;
    font-size: 12px;
    color: #7a8794;
  }
  .actions {
    display: flex;
    gap: 10px;
    align-items: center;
  }
  .save {
    background: #1d2836;
    color: #eaf2fb;
    border: 1px solid #2b3a4e;
    border-radius: 10px;
    padding: 10px 16px;
    font-size: 14px;
    cursor: pointer;
  }
  .save:hover {
    background: #24344a;
  }
  .save.big {
    background: #2f6fd8;
    border-color: #2f6fd8;
    font-weight: 600;
    padding: 12px 22px;
  }
  .save.big:hover {
    background: #3c7ce2;
  }
  .toast {
    margin-top: 14px;
    padding: 10px 14px;
    border-radius: 8px;
    font-size: 13px;
  }
  .toast.ok {
    background: #12271a;
    color: #8fd6a0;
    border: 1px solid #1d4029;
  }
  .toast.err {
    background: #2b1417;
    color: #e39aa2;
    border: 1px solid #4a2027;
  }
  .grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(220px, 1fr));
    gap: 14px;
  }
  .clip {
    text-align: left;
    background: #161b22;
    border: 1px solid #1f2630;
    border-radius: 10px;
    overflow: hidden;
    padding: 0;
    cursor: pointer;
    color: inherit;
    font: inherit;
  }
  .clip:hover {
    border-color: #2b3a4e;
  }
  .clip img {
    width: 100%;
    aspect-ratio: 16/9;
    object-fit: cover;
    display: block;
  }
  .nothumb {
    width: 100%;
    aspect-ratio: 16/9;
    display: grid;
    place-items: center;
    color: #55616e;
    font-size: 12px;
    background: #0e1116;
  }
  .meta {
    padding: 8px 10px 10px;
  }
  .name {
    font-size: 12px;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .sub {
    font-size: 11px;
    color: #7a8794;
    margin-top: 3px;
  }
  .empty {
    color: #7a8794;
    font-size: 13px;
    padding: 18px;
  }
</style>

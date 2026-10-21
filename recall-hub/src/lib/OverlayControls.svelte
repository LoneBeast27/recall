<script lang="ts">
  import { onMount } from 'svelte';
  import { invoke } from '@tauri-apps/api/core';
  type OverlayStatus={enabled:boolean;corner:string;running:boolean;error:string|null};
  let status=$state<OverlayStatus>({enabled:false,corner:'top-right',running:false,error:null});
  let busy=$state(false);let error=$state('');
  const update=async(enabled:boolean,corner=status.corner)=>{if(busy)return;busy=true;error='';try{status=await invoke<OverlayStatus>('overlay_configure',{enabled,corner});}catch(reason){error=String(reason);}finally{busy=false;}};
  onMount(()=>{void invoke<OverlayStatus>('overlay_status').then(value=>status=value).catch(reason=>error=String(reason));});
</script>
<div class="overlay-controls">
  <button class:enabled={status.enabled&&status.running} disabled={busy} aria-pressed={status.enabled} onclick={()=>update(!status.enabled)} title="Show brief clip-saved, microphone and error notifications">
    <svg viewBox="0 0 20 20" aria-hidden="true"><rect x="2.5" y="3.5" width="15" height="12" rx="2"/><path d="M6 7h5m-5 3h8"/></svg>
    <span>Overlay</span><span class="switch">{busy?'…':status.enabled?'On':'Off'}</span>
  </button>
  {#if status.enabled}
    <select aria-label="Overlay position" value={status.corner} disabled={busy} onchange={e=>update(true,e.currentTarget.value)}>
      <option value="top-right">Top right</option><option value="top-left">Top left</option><option value="bottom-right">Bottom right</option><option value="bottom-left">Bottom left</option>
    </select>
  {/if}
  {#if error||status.error}<p role="status">{error||status.error}</p>{/if}
</div>
<style>
.overlay-controls{display:grid;gap:6px;min-width:0}.overlay-controls button{display:flex;align-items:center;gap:8px;width:100%;padding:8px;background:transparent;border:1px solid var(--line,#343e4c);border-radius:4px;color:var(--muted,#9aa6b5);font:inherit;font-size:12px;cursor:pointer}.overlay-controls svg{width:17px;height:17px;fill:none;stroke:currentColor;stroke-width:1.4}.overlay-controls .switch{margin-left:auto;font-size:11px}.overlay-controls button.enabled{color:var(--text,#eef1f5);border-color:#536e82}.overlay-controls button:focus-visible,.overlay-controls select:focus-visible{outline:2px solid #7ea7d8;outline-offset:2px}.overlay-controls button:disabled{opacity:.5}.overlay-controls select{width:100%;background:var(--panel,#1d222a);border:1px solid var(--line,#343e4c);color:var(--muted,#9aa6b5);padding:6px;font:inherit;font-size:11px;border-radius:4px}.overlay-controls p{font-size:11px;line-height:1.4;color:#d98e93;margin:0;overflow-wrap:anywhere}
</style>


<script lang="ts">
  import { onMount } from 'svelte';
  import { listen } from '@tauri-apps/api/event';
  import { invoke } from '@tauri-apps/api/core';

  interface Meter {
    label: string;
    rms_db?: number;
    peak_db?: number;
    missing?: boolean;
    error?: string;
  }

  const ORDER = ['Desktop audio', 'Microphone'];
  let meters: Record<string, Meter> = $state({});
  let ports = $state<{ inputs: string[]; outputs: string[] }>({ inputs: [], outputs: [] });
  let midiLog: string[] = $state([]);
  let midiNote = $state(60);
  let midiChannel = $state(1);
  let midiResult = $state('');

  onMount(async () => {
    await listen<Meter>('vu', (e) => {
      meters[e.payload.label] = e.payload;
    });
    await listen<Record<string, unknown>>('midi-in', (e) => {
      midiLog = [JSON.stringify(e.payload), ...midiLog].slice(0, 10);
    });
    try {
      ports = await invoke('midi_ports');
    } catch {
      /* midi unavailable */
    }
  });

  const pct = (db?: number) =>
    db === undefined ? 0 : Math.max(0, Math.min(100, ((db + 60) / 60) * 100));

  // Target the first hardware output (skip the Windows GS software synth).
  const hwOut = $derived(
    ports.outputs.find((o) => !o.toLowerCase().includes('wavetable')) ?? ''
  );

  const sendNote = async () => {
    midiResult = '';
    if (!hwOut) {
      midiResult = 'no hardware MIDI output found';
      return;
    }
    const ch = (midiChannel - 1) & 0x0f;
    try {
      await invoke('midi_send', {
        portMatch: hwOut,
        bytes: [0x90 | ch, midiNote & 0x7f, 0x7f],
      });
      await new Promise((r) => setTimeout(r, 120));
      await invoke('midi_send', {
        portMatch: hwOut,
        bytes: [0x80 | ch, midiNote & 0x7f, 0],
      });
      midiResult = `sent note ${midiNote} ch ${midiChannel} -> ${hwOut}`;
    } catch (e) {
      midiResult = `failed: ${e}`;
    }
  };
</script>

<h1>Audio</h1>

<h2>Cable meters</h2>
<section class="panel">
  {#each ORDER as label}
    {@const m = meters[label]}
    <div class="meter" class:missing={m?.missing}>
      <div class="mlabel">{label}</div>
      <div class="mbar">
        <div class="mfill" style="width:{pct(m?.rms_db)}%"></div>
        <div class="mpeak" style="left:{pct(m?.peak_db)}%"></div>
      </div>
      <div class="mdb">
        {m?.missing ? 'not found' : m ? `${m.rms_db?.toFixed(0)} dB` : '…'}
      </div>
    </div>
  {/each}
</section>

<h2>MIDI</h2>
<section class="panel">
  <div class="sub">
    outputs: {ports.outputs.length ? ports.outputs.join(' · ') : 'none found'}<br />
    inputs: {ports.inputs.length ? ports.inputs.join(' · ') : 'none found'}
  </div>
  <div class="midirow">
    <label>note <input type="number" bind:value={midiNote} min="0" max="127" /></label>
    <label>ch <input type="number" bind:value={midiChannel} min="1" max="16" /></label>
    <button class="act" onclick={sendNote}>Send test note</button>
    {#if midiResult}<span class="sub">{midiResult}</span>{/if}
  </div>
  <div class="sub" style="margin-top:10px">
    Incoming messages from the first MIDI input appear below.
  </div>
  {#if midiLog.length}
    <pre class="log">{midiLog.join('\n')}</pre>
  {/if}
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
  .meter {
    display: flex;
    align-items: center;
    gap: 14px;
    padding: 6px 0;
  }
  .meter.missing {
    opacity: 0.35;
  }
  .mlabel {
    width: 90px;
    font-size: 13px;
    color: #9aa7b5;
  }
  .mbar {
    flex: 1;
    position: relative;
    height: 12px;
    background: #0e1116;
    border-radius: 6px;
    overflow: hidden;
  }
  .mfill {
    height: 100%;
    background: linear-gradient(90deg, #2e9e5b, #7fd68f);
    transition: width 0.1s linear;
  }
  .mpeak {
    position: absolute;
    top: 0;
    width: 2px;
    height: 100%;
    background: #ffd479;
    transition: left 0.1s linear;
  }
  .mdb {
    width: 64px;
    text-align: right;
    font-size: 12px;
    color: #7a8794;
    font-variant-numeric: tabular-nums;
  }
  .sub {
    font-size: 12px;
    color: #7a8794;
  }
  .midirow {
    display: flex;
    align-items: center;
    gap: 14px;
    margin-top: 12px;
  }
  .midirow label {
    font-size: 12px;
    color: #9aa7b5;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .midirow input {
    width: 64px;
    background: #0e1116;
    color: #dbe2ea;
    border: 1px solid #2b3a4e;
    border-radius: 6px;
    padding: 6px 8px;
  }
  .act {
    background: #1d2836;
    color: #eaf2fb;
    border: 1px solid #2b3a4e;
    border-radius: 10px;
    padding: 8px 14px;
    font-size: 13px;
    cursor: pointer;
  }
  .act:hover {
    background: #24344a;
  }
  .log {
    margin-top: 10px;
    background: #0e1116;
    border-radius: 8px;
    padding: 10px;
    font-size: 11px;
    color: #8fa4b8;
    max-height: 160px;
    overflow-y: auto;
  }
</style>

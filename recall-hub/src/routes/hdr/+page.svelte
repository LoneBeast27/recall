<script lang="ts">
  import { hdr, engine, toggleHdr, setMode, setAcProfile } from '$lib/daemon';

  let profileNote = $state('');

  const switchProfile = async (v: 'punchy' | 'accurate') => {
    profileNote = '';
    try {
      await setAcProfile(v);
      profileNote = `${v} set as the Advanced-Color default — takes effect on the next HDR entry.`;
    } catch (e) {
      profileNote = `failed: ${e}`;
    }
  };
</script>

<h1>HDR</h1>

<section class="panel">
  {#if $hdr}
    <div class="staterow">
      <div class="state" class:on={$hdr.hdr}>
        {$hdr.hdr ? 'HDR' : 'SDR'}
      </div>
      <div class="why">
        <div><b>{$hdr.target}</b></div>
        <div class="sub">
          {#if $hdr.session_pid}
            game session (pid {$hdr.session_pid}) owns the display
          {:else if $hdr.manual_pin}
            manual pin active
          {:else}
            auto policy ({$hdr.auto ? 'on' : 'off'}) · desktop baseline
          {/if}
          · engine {$hdr.engine_health}
        </div>
      </div>
      <button class="act" onclick={toggleHdr}>Toggle HDR</button>
    </div>
  {:else}
    <div class="sub">waiting for daemon…</div>
  {/if}
</section>

<h2>Capture tonemap</h2>
<section class="panel">
  <div class="sub" style="margin-bottom:10px">
    Engine mode selector (what the SDR output window / clips do):
    currently <b>{$engine?.select ?? '–'}</b> → {$engine?.mode ?? '–'}
  </div>
  <div class="btnrow">
    <button class="act" onclick={() => setMode('auto')}>Auto</button>
    <button class="act" onclick={() => setMode('sdr')}>Force SDR</button>
    <button class="act" onclick={() => setMode('hdr')}>Force tonemap</button>
  </div>
</section>

<h2>SDR-in-HDR profile (MHC2)</h2>
<section class="panel">
  <div class="sub" style="margin-bottom:10px">
    Which gamut the Advanced-Color pipeline assumes for SDR content while HDR is on.
  </div>
  <div class="btnrow">
    <button class="act" onclick={() => switchProfile('punchy')}>Punchy (native stretch)</button>
    <button class="act" onclick={() => switchProfile('accurate')}>Accurate (true sRGB)</button>
  </div>
  {#if profileNote}
    <div class="note">{profileNote}</div>
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
  .staterow {
    display: flex;
    align-items: center;
    gap: 18px;
  }
  .state {
    font-size: 22px;
    font-weight: 700;
    padding: 14px 20px;
    border-radius: 10px;
    background: #0e1116;
    color: #9aa7b5;
  }
  .state.on {
    color: #ffd479;
    background: #2a2113;
  }
  .why {
    flex: 1;
  }
  .sub {
    font-size: 12px;
    color: #7a8794;
    margin-top: 3px;
  }
  .btnrow {
    display: flex;
    gap: 10px;
  }
  .act {
    background: #1d2836;
    color: #eaf2fb;
    border: 1px solid #2b3a4e;
    border-radius: 10px;
    padding: 10px 16px;
    font-size: 14px;
    cursor: pointer;
  }
  .act:hover {
    background: #24344a;
  }
  .note {
    margin-top: 12px;
    font-size: 12px;
    color: #8fd6a0;
  }
</style>

<script lang="ts">let { name }: { name: 'clips' | 'editor' | 'hdr' | 'audio' } = $props();</script>
<svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d={name === 'clips' ? 'M4 6h11v12H4zM15 10l5-3v10l-5-3z' : name === 'editor' ? 'M4 17.5V20h2.5L18 8.5 15.5 6zM14.7 6.8l2.5 2.5' : name === 'hdr' ? 'M12 3a9 9 0 1 0 9 9c0-5-4-9-9-9Zm0 3v12a6 6 0 0 1 0-12Z' : 'M5 9v6M9 6v12M13 9v6M17 4v16M21 9v6'} /></svg>
<style>svg{width:18px;height:18px;fill:none;stroke:currentColor;stroke-width:1.7;stroke-linecap:round;stroke-linejoin:round;flex:none}</style>

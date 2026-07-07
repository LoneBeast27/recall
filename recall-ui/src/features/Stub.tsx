const COPY: Record<string, { title: string; body: string }> = {
  clipper: {
    title: "Clipper",
    body: "Instant replay buffer → CFR NVENC clips with capture-feedback overlay. Coming soon.",
  },
  gallery: {
    title: "Gallery",
    body: "Adobe-style clip preview: scrub, star, quick-trim, multi-track audio. Coming soon.",
  },
  overlay: {
    title: "Overlay",
    body: "On-screen capture-feedback overlay configuration. Coming soon.",
  },
};

export function Stub({ id }: { id: string }) {
  const c = COPY[id] ?? { title: id, body: "Coming soon." };
  return (
    <div className="stub">
      <h2>{c.title}</h2>
      <p>{c.body}</p>
    </div>
  );
}

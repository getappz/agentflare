(() => {
  const offset = __OFFSET__;
  const all = window.__agentflareTranscript?.segments;
  if (!all) throw new Error('Transcript state was lost');
  const segments = [];
  let next = offset;
  while (next < all.length && segments.length < 100) {
    const candidate = [...segments, all[next]];
    if (JSON.stringify({ offset, next: next + 1, segments: candidate }).length > 7000) break;
    segments.push(all[next++]);
  }
  if (next === offset) throw new Error('Caption segment exceeds browser chunk limit');
  return JSON.stringify({ offset, next, segments });
})()

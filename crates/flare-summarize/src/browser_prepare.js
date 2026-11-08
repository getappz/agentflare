/* prepare */
(async () => {
  const languages = __LANGUAGES__;
  const pause = () => new Promise(resolve => setTimeout(resolve, 250));
  const waitFor = async fn => {
    for (let i = 0; i < 40; i++) {
      const value = fn();
      if (value) return value;
      await pause();
    }
    throw new Error('YouTube transcript did not become available (consent, login, or missing captions)');
  };
  const player = await waitFor(() => window.ytInitialPlayerResponse
    || document.querySelector('#movie_player')?.getPlayerResponse?.()
    || document.querySelector('ytd-watch-flexy')?.playerData);
  const tracks = player.captions?.playerCaptionsTracklistRenderer?.captionTracks || [];
  const track = languages.map(language => tracks.find(t => t.languageCode === language)
    || tracks.find(t => t.languageCode.split('-')[0] === language.split('-')[0])).find(Boolean);
  if (!track) throw new Error('No captions in a requested language: ' + languages.join(', '));
  let segments;
  // A request in the rendered page can succeed when the standalone HTTP client fails.
  try {
    const url = new URL(track.baseUrl);
    if (url.protocol !== 'https:' || !['www.youtube.com', 'youtube.com'].includes(url.hostname)) {
      throw new Error('Unexpected caption URL');
    }
    url.searchParams.set('fmt', 'json3');
    const response = await fetch(url.href, { signal: AbortSignal.timeout(10000) });
    if (!response.ok) throw new Error('Caption request failed');
    const data = await response.json();
    segments = (data.events || []).filter(e => e.segs).map(e => ({
      start: e.tStartMs / 1000,
      text: e.segs.map(s => s.utf8 || '').join('').trim()
    })).filter(s => s.text);
  } catch { /* Use the transcript panel below. */ }
  if (!segments?.length) {
    document.querySelector('ytd-watch-metadata #description-inline-expander #expand')?.click();
    const button = await waitFor(() => [...document.querySelectorAll('button')].find(b =>
      /^(show transcript)$/i.test((b.getAttribute('aria-label') || b.textContent || '').trim())));
    button.click();
    const panel = await waitFor(() => document.querySelector('ytd-transcript-renderer'));
    const trackName = track.name?.simpleText || track.name?.runs?.map(r => r.text).join('');
    if (tracks.length > 1) {
      const menu = panel.querySelector('ytd-transcript-footer-renderer yt-dropdown-menu');
      if (!menu || !trackName) throw new Error('Cannot verify transcript panel language');
      menu.click();
      const option = await waitFor(() => [...menu.querySelectorAll('tp-yt-paper-item')]
        .find(item => item.textContent.trim() === trackName));
      const previous = panel.querySelector('ytd-transcript-segment-list-renderer')?.data;
      const alreadySelected = menu.querySelector('#label')?.textContent.trim() === trackName;
      option.click();
      await waitFor(() => menu.querySelector('#label')?.textContent.trim() === trackName);
      if (!alreadySelected) {
        await waitFor(() => panel.querySelector('ytd-transcript-segment-list-renderer')?.data !== previous);
      }
    }
    await waitFor(() => panel.querySelector('ytd-transcript-segment-renderer'));
    // Read the renderer's complete segment list rather than a possibly virtualized DOM slice.
    const renderer = panel.querySelector('ytd-transcript-segment-list-renderer');
    const initialSegments = renderer?.data?.initialSegments;
    if (!initialSegments?.length || renderer.data.continuations?.length
      || initialSegments.some(item => item.continuationItemRenderer)) {
      throw new Error('Cannot verify complete transcript panel data');
    }
    segments = initialSegments.map(item => item.transcriptSegmentRenderer).filter(Boolean).map(s => ({
      start: Number(s.startMs) / 1000,
      text: (s.snippet?.runs?.map(r => r.text).join('') || s.snippet?.simpleText || '').trim()
    })).filter(s => s.text);
  }
  if (!segments.length) throw new Error('YouTube returned empty captions');
  window.__agentflareTranscript = { segments, language: track.languageCode, generated: track.kind === 'asr' };
  return JSON.stringify({ count: segments.length, language: track.languageCode, generated: track.kind === 'asr' });
})()

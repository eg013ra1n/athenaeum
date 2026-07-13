/**
 * Only ever hand http(s) URLs to the OS browser. `openUrl` reaches
 * plugin-opener on desktop, so a `javascript:`/`file:`/`data:` URL would be a
 * real vector — and every candidate here traces back to the settings-configurable
 * hub URL (portalBase, or the deep link minted by `create_collab_link_intent`).
 * Returns the normalized URL, or null when it is not a plain web address.
 */
export function safeExternalUrl(raw: string): string | null {
  try {
    const u = new URL(raw);
    return u.protocol === 'https:' || u.protocol === 'http:' ? u.toString() : null;
  } catch {
    return null;
  }
}

/**
 * The one built-in hoster plugin. Every other plugin is user-loadable
 * (recipes or scripts - see the Plugins options page).
 *
 * generic: any http(s) link whose path ends in a known archive/media/disc
 * extension is a download; the filename is the URL tail. It has no resolve()
 * - direct links need none - so plugins stay an enhancement, not a gate.
 */

const EXT_RE = /\.(zip|rar|7z|tar|gz|bz2|xz|zst|iso|bin|mdf|exe|msi|dmg|apk|mp4|mkv|webm|mov|avi|flv|m4v|ts|mp3|m4a|aac|opus|ogg|flac|epub|pdf)([?#]|$)/i;

export default {
    id: "generic",
    label: "Direct links (built-in)",

    match(url) {
        try {
            const u = new URL(url);
            if (u.protocol !== "http:" && u.protocol !== "https:") return false;
            return EXT_RE.test(u.pathname);
        } catch {
            return false;
        }
    },

    name(url) {
        try {
            const tail = decodeURIComponent(new URL(url).pathname.split("/").pop() || "");
            return tail || null;
        } catch {
            return null;
        }
    },
};

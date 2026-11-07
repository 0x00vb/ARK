// A small picture of an image in the project: a chip in the composer, an image under a sent
// message. With `url` (a blob made from the file just chosen) it shows at once; without, it
// reads the file (`file_media`). The blob is freed once drawn, unless `keep` says another
// element still needs it.

import { h } from "./dom.js";

export function thumb(api, { path, url }, { keep = false } = {}) {
  const img = h("img.thumb", { alt: "", draggable: "false", hidden: true });
  const show = (src, free) => {
    img.onload = () => {
      if (free) URL.revokeObjectURL(src);
      img.hidden = false;
    };
    img.onerror = () => {
      if (free) URL.revokeObjectURL(src);
    };
    img.src = src;
  };
  if (url) {
    show(url, !keep);
    return img;
  }
  api.call("file_media", { path }).then((m) => {
    const bytes = Uint8Array.from(atob(m.data), (c) => c.charCodeAt(0));
    show(URL.createObjectURL(new Blob([bytes], { type: m.media_type })), true);
  }).catch(() => {
    // A file that is gone or unreadable leaves its tile empty.
    img.dataset.failed = "true";
  });
  return img;
}

export const baseName = (path) => String(path).split("/").pop();

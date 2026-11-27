import { node, svg } from "./dom";
import { invoke } from "./ipc";
import type { Candidate } from "./ipc";

export function leftoverDetails(item: Candidate) {
  const details = node("details", undefined, "leftover-details");
  const folder = item.file_type === "Folder" && item.selectable;
  details.append(
    node("summary", folder ? "Contents and details" : "Details"),
    node("p", item.reason, "reason"),
  );
  if (folder) loadFolderContents(details, item.path, []);
  return details;
}

function loadFolderContents(
  details: HTMLDetailsElement,
  path: string,
  folders: string[],
) {
  const list = node("ul", undefined, "leftover-contents");
  const status = node("p", undefined, "muted");
  status.setAttribute("role", "status");
  details.append(list, status);
  let loading = false;
  let loaded = false;
  details.addEventListener("toggle", async () => {
    if (!details.open || loading || loaded) return;
    loading = true;
    status.textContent = "Reading folder…";
    try {
      const result = await invoke("leftover_contents", { path, folders });
      for (const entry of result.entries) {
        const row = node("li");
        if (entry.browsable) {
          const subfolder = node("details");
          subfolder.append(node("summary", entry.name));
          loadFolderContents(subfolder, path, [...folders, entry.name]);
          row.append(subfolder);
        } else {
          row.append(
            svg(entry.folder ? "folder-icon" : "files"),
            node("span", entry.name),
          );
        }
        list.append(row);
      }
      status.textContent = result.more
        ? "Open the folder to see all contents."
        : result.entries.length
          ? ""
          : "This folder is empty.";
      status.hidden = !status.textContent;
      loaded = true;
    } catch (error) {
      status.textContent = String(error);
    } finally {
      loading = false;
    }
  });
}

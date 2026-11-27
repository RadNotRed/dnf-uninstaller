export type Theme = "system" | "light" | "dark";

export interface Application {
  key: string;
  name: string;
  source: string;
  version: string;
  id: string;
  summary: string;
  origin: string;
  launcher: string | null;
  removable: boolean;
  launcher_removable: boolean;
}

type Kind =
  | "Launcher"
  | "Icon"
  | "FlatpakData"
  | "Config"
  | "Cache"
  | "Data"
  | "State"
  | "AppImage"
  | "ToolboxBin"
  | "StandaloneBinary";

export interface Candidate {
  path: string;
  kind: Kind;
  reason: string;
  confirmed: boolean;
  selectable: boolean;
  bytes: number | null;
  file_type: string;
}

export interface Recovery {
  id: string;
  path: string;
  kind: Kind;
}

export interface UninstallHistory {
  entries: {
    name: string;
    app_id: string;
    source: string;
    version: string;
    removed_at: number | null;
  }[];
  earlier_ids: string[];
}

export interface Scan<T> {
  items: T[];
  warnings: string[];
}

export interface Confirmation {
  token: number;
  items: string[];
  explanation: string;
  authorization: string | null;
}

export interface ResultReport {
  completed: number;
  warnings: string[];
}

interface Configuration {
  theme: Theme;
  dark: boolean | null;
  warning: string | null;
  rpm_helper: boolean;
  update_repo: boolean;
  version: string;
  about_icon: string;
}

type Commands = {
  app_icons: [{ keys: string[] }, Partial<Record<string, string>>];
  cancel_scan: [undefined, void];
  check_updates: [{ refresh: boolean }, string | null];
  configuration: [undefined, Configuration];
  confirm: [{ token: number }, ResultReport];
  installed: [{ refresh: boolean }, Scan<Application>];
  leftover_contents: [
    { path: string; folders: string[] },
    {
      entries: { name: string; folder: boolean; browsable: boolean }[];
      more: boolean;
    },
  ];
  open_leftover: [{ path: string }, void];
  preview_cleanup: [
    {
      paths: string[];
      launchers: boolean;
      recent?: boolean;
      permanent?: boolean;
    },
    Confirmation,
  ];
  preview_delete: [{ id: string }, Confirmation];
  preview_remove_launcher: [{ key: string }, Confirmation];
  preview_uninstall: [{ key: string }, Confirmation];
  preview_update: [{ version: string }, Confirmation];
  recovery: [undefined, Recovery[]];
  uninstall_history: [undefined, UninstallHistory];
  restore: [{ id: string }, ResultReport];
  save_theme: [{ theme: Theme }, void];
  scan_cleanup: [
    { launchers: boolean; refresh: boolean; recent?: boolean },
    Scan<Candidate>,
  ];
  show_window: [undefined, void];
};

type Events = {
  "system-theme": boolean | null;
  "operation-progress": string;
};

declare global {
  interface Window {
    __TAURI__: {
      core: {
        invoke<T>(command: string, args?: object): Promise<T>;
      };
      event: {
        listen<T>(
          name: string,
          handler: (event: { payload: T }) => void,
        ): Promise<() => void>;
      };
    };
  }
}

export function invoke<C extends keyof Commands>(
  command: C,
  ...args: Commands[C][0] extends undefined ? [] : [Commands[C][0]]
): Promise<Commands[C][1]> {
  return window.__TAURI__.core.invoke(command, args[0]);
}

export function listen<E extends keyof Events>(
  name: E,
  handler: (event: { payload: Events[E] }) => void,
): Promise<() => void> {
  return window.__TAURI__.event.listen(name, handler);
}

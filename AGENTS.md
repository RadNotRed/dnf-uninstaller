# AI DEVELOPMENT RULES FOR RUST + TAURI 2

Apply these rules to all implementation, UI design, refactoring, and review work. Treat them as defaults unless an explicit project requirement overrides them.

## 1. VISUAL DESIGN

- Do not use gradients unless explicitly requested. Prefer flat, solid colors.
- Do not default to purple, violet, neon blue, or stereotypical AI-generated color schemes.
- Do not use glassmorphism, excessive blur, glowing borders, or decorative shadows without a functional reason.
- Do not add random text, filler paragraphs, motivational slogans, or explanatory subtitles to fill empty space.
- Do not use emojis as icons. Use one consistent SVG icon library.
- Do not use huge border radii on every element. Use a consistent, restrained radius scale.
- Do not wrap everything in cards. Use whitespace, separators, and hierarchy where appropriate.
- Do not add decorative charts, fake statistics, or placeholder metrics.
- Do not use oversized headings or excessive font weights.
- Do not use multiple competing accent colors.
- Do not create unnecessarily large, empty layouts.
- Do not make every button the same visual priority.
- Do not create custom UI controls when accessible native or established components already exist.
- Do not use animations merely to make the interface feel modern.

## 2. SPACING AND ALIGNMENT

- Do not align elements by eye or with arbitrary offsets. Use measured CSS layout rules.
- Do not fix alignment problems using negative margins, absolute positioning, or transforms unless genuinely required.
- Do not use random pixel values for spacing. Use a consistent spacing scale, such as 4, 8, 12, 16, 24, and 32 CSS pixels.
- Do not mix inconsistent padding, margins, gaps, or control heights.
- Do not use fixed positioning for ordinary page layouts. Prefer Flexbox and CSS Grid.
- Do not stretch buttons, inputs, or cards unnecessarily.
- Do not ignore optical alignment of icons and text.
- Do not let text wrap unpredictably or overlap adjacent controls.
- Do not rely on a single screen resolution or window size.
- Do not allow horizontal overflow unless the component intentionally supports it.
- Do not use magic numbers to hide underlying layout problems.
- Do not sacrifice alignment consistency just to match a screenshot superficially.

## 3. LIGHT AND DARK MODE

- Do not implement dark mode as a simple inversion of light mode.
- Do not hardcode colors throughout individual components. Use semantic design tokens and CSS variables.
- Do not support only one theme. Implement light, dark, and system-following modes.
- Do not ignore operating-system theme changes.
- Do not reset the user's selected theme on every launch.
- Do not use pure black or pure white everywhere without considering contrast and comfort.
- Do not use low-contrast gray text that becomes difficult to read.
- Do not forget to style dialogs, dropdowns, scrollbars, tooltips, and notifications in both themes.
- Do not introduce a bright flash during app startup while the saved theme loads.
- Do not make hover, focus, disabled, or selected states indistinguishable between themes.

## 4. TYPOGRAPHY AND CONTENT

- Do not mix multiple font families without a clear reason.
- Do not use tiny text for essential information.
- Do not center-align long paragraphs, tables, forms, or settings descriptions.
- Do not write long descriptions for obvious controls.
- Do not use vague labels such as 'Click Here', 'Submit', or 'Do Thing'.
- Do not use technical jargon in user-facing messages unless the audience needs it.
- Do not generate fake content and present it as real.
- Do not truncate important information without an accessible way to view the full value.
- Do not use excessive uppercase text.
- Do not rely on placeholder text as a replacement for form labels.
- Do not add unnecessary tooltips to self-explanatory elements.

## 5. INTERACTIONS AND USABILITY

- Do not create buttons, toggles, menu items, or settings that do nothing.
- Do not use fake interactions that only change the appearance of a control.
- Do not show a success notification before an operation actually succeeds.
- Do not silently ignore errors.
- Do not use blocking alerts for ordinary feedback.
- Do not show unnecessary confirmation dialogs for harmless actions.
- Do not perform destructive operations without appropriate confirmation or recovery.
- Do not reset unsaved user input when navigating between views.
- Do not leave buttons enabled while duplicate submissions could cause problems.
- Do not implement hover-only functionality that cannot be accessed by keyboard.
- Do not create hidden navigation with no visual indication.
- Do not use loading spinners for instant operations.
- Do not omit loading, empty, error, and success states when they are relevant.
- Do not use animations that delay interaction or obscure the result of an action.
- Do not require multiple clicks for actions that can safely be completed in one.
- Do not hijack familiar keyboard shortcuts.
- Do not ignore keyboard navigation, focus indicators, and screen-reader labels.

## 6. DESKTOP APPLICATION BEHAVIOR

- Do not design the app like a mobile website stretched across a desktop window.
- Do not imitate a web landing page when building an application workspace.
- Do not add a giant hero section to a utility or settings app.
- Do not force a sidebar when simpler navigation is sufficient.
- Do not waste desktop screen space on oversized controls.
- Do not assume the app is always maximized.
- Do not let the layout break when the user resizes the window.
- Do not create a custom title bar unless there is a clear need.
- Do not break native window controls, dragging, resizing, or keyboard behavior.
- Do not disregard Windows, Linux, and macOS platform conventions.
- Do not assume all operating systems use the same fonts, scrollbars, or window decorations.
- Do not assume all displays use the same DPI or scaling factor.
- Do not open unnecessary windows for simple actions.
- Do not ignore window position and size persistence when appropriate.
- Do not trap the user in a window that cannot be resized or closed normally.

## 7. TAURI AND RUST BACKEND

- Do not use Tauri v1 APIs, configuration formats, or plugin instructions in a Tauri v2 project.
- Do not move privileged operating-system operations into frontend JavaScript when they belong in Rust.
- Do not expose unrestricted filesystem, shell, or system access to the frontend.
- Do not grant broad Tauri capabilities or plugin permissions when narrower permissions are sufficient.
- Do not assume every registered custom Rust command is restricted by plugin permissions; review custom command exposure separately.
- Do not trust frontend input. Validate paths, arguments, identifiers, and other inputs in Rust.
- Do not execute arbitrary shell commands constructed from untrusted strings.
- Do not block the UI thread with expensive synchronous operations.
- Do not confuse async functions with automatically nonblocking CPU-bound work.
- Do not use unwrap() or expect() on routine fallible operations where errors should be handled.
- Do not suppress Rust errors or discard Result values without a justified reason.
- Do not use unsafe Rust unless necessary and documented.
- Do not use excessive cloning or shared mutable state without understanding ownership and concurrency.
- Do not expose secrets, API tokens, credentials, or sensitive paths in logs.
- Do not store sensitive credentials in ordinary plaintext configuration files.
- Do not add Rust crates or JavaScript dependencies without a real need.
- Do not duplicate backend business logic in the frontend.
- Do not use polling when an appropriate event-driven mechanism exists.
- Do not leak Tauri event listeners or leave background tasks running unnecessarily.
- Do not ignore application shutdown, cancellation, or cleanup.
- Do not assume file paths, environment variables, or executable locations are identical across operating systems.
- Do not create unbounded background tasks, threads, or worker pools.

## 8. FRONTEND CODE QUALITY

- Do not write the entire application in one component or one giant file.
- Do not split tiny components into separate files without a maintainability reason.
- Do not duplicate UI components, CSS rules, or business logic.
- Do not introduce a large state-management library for a small amount of state.
- Do not use deeply nested conditional rendering when a simpler structure works.
- Do not use inline styles for everything.
- Do not litter the codebase with arbitrary z-index values.
- Do not use !important to cover up CSS architecture problems.
- Do not add speculative abstractions or future-proofing that the current app does not need.
- Do not rewrite unrelated code when implementing a focused feature.
- Do not silently remove existing functionality.
- Do not leave unused dependencies, imports, components, or debug statements.
- Do not use placeholder functions or TODO implementations while claiming a feature is complete.
- Do not introduce inconsistent naming conventions.
- Do not manipulate the DOM manually when the frontend framework should manage it.

## 9. PERFORMANCE

- Do not load large resources during startup unless they are immediately required.
- Do not perform expensive operations on every render.
- Do not continuously poll backend state when events or targeted updates are appropriate.
- Do not re-render large lists unnecessarily.
- Do not render thousands of rows at once when virtualization is appropriate.
- Do not create excessive visual effects that increase CPU or GPU usage.
- Do not leave timers, subscriptions, or observers active after their components are destroyed.
- Do not perform synchronous filesystem or network operations on latency-sensitive execution paths.
- Do not introduce a heavyweight dependency for functionality already available in Rust, Tauri, or the web platform.
- Do not store unnecessarily large datasets in frontend state.
- Do not sacrifice startup speed for decorative features.

## 10. ACCESSIBILITY

- Do not communicate meaning through color alone.
- Do not remove visible keyboard focus indicators.
- Do not make essential interactions dependent on hover.
- Do not disable text selection globally.
- Do not prevent normal keyboard navigation.
- Do not use icon-only buttons without accessible labels.
- Do not ignore text scaling or system display scaling.
- Do not force animations when reduced-motion settings are enabled.
- Do not create controls with unnecessarily small click targets.
- Do not use insufficient color contrast.
- Do not replace semantic elements with generic div elements unnecessarily.

## 11. SETTINGS, STORAGE, AND DATA

- Do not implement settings that appear to save but reset after restarting the app.
- Do not overwrite user configuration files without validation or a recovery strategy.
- Do not persist temporary UI state unless it benefits the user.
- Do not silently discard user changes.
- Do not assume configuration files always exist or contain valid data.
- Do not store application settings in arbitrary working-directory locations.
- Do not mix cache, configuration, and user data without distinguishing their purposes.
- Do not perform automatic migrations without considering existing data and failure recovery.
- Do not erase or reset user data to work around a development error.
- Do not leave settings controls disconnected from their actual backend behavior.

## 12. TESTING AND COMPLETION

- Do not claim a feature works solely because the code was written.
- Do not claim tests passed without running them.
- Do not ignore compiler errors, type errors, warnings, or failed checks.
- Do not assume a successful frontend build means the Tauri application builds successfully.
- Do not skip testing narrow window sizes and both themes.
- Do not leave broken keyboard interactions or inaccessible controls.
- Do not modify unrelated working features to fix a visual issue.
- Do not generate mock data in production interfaces unless clearly identified as demonstration data.
- Do not claim cross-platform compatibility without appropriate validation.
- Do not hide incomplete functionality behind polished visuals.
- Do not stop at an attractive screenshot; verify real interactions and state changes.

## FINAL EXPECTATIONS

Build a clean, restrained, native-feeling desktop app with consistent spacing, functional controls, light/dark/system themes, safe Rust integration, and good performance. Prefer simple, maintainable implementations over decorative complexity. Preserve existing features. Verify actual behavior and report what was and was not tested.


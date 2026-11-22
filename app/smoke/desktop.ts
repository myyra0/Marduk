/**
 * Desktop shell smoke lane.
 *
 * Proves the built Tauri shell starts under a real WebKit webview, loads the
 * bundled frontend, renders the authoritative status view, and answers one
 * typed read-only command over its own IPC transport. There is no Vite dev
 * server, no mocked transport and no skipped attach: if the WebDriver cannot
 * start the app, this script fails.
 *
 * `status` is invoked on a fresh catalog root, so the expected result is the
 * real empty workspace with deploy and restore typed as unavailable. It never
 * asserts a deploy success.
 */

import { existsSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const driverUrl = process.env.MARDUK_DRIVER_URL ?? 'http://127.0.0.1:4444';
const here = dirname(fileURLToPath(import.meta.url));
const binary =
  process.env.MARDUK_DESKTOP_BINARY ??
  resolve(
    here,
    `../src-tauri/target/debug/marduk-desktop${process.platform === 'win32' ? '.exe' : ''}`,
  );
const attachTimeoutMs = Number(process.env.MARDUK_ATTACH_TIMEOUT_MS ?? 90_000);
const uiTimeoutMs = Number(process.env.MARDUK_UI_TIMEOUT_MS ?? 90_000);

/** Evaluated in the page. Reports only what the shell actually rendered. */
const DOM_STATE = `
  const root = document.getElementById('root');
  const shell = root !== null && root.querySelector('main.shell') !== null;
  const heading = document.querySelector('h1');
  const text = document.body ? document.body.innerText : '';
  const rows = Array.from(document.querySelectorAll('.availability')).map(function (row) {
    return row.textContent ? row.textContent.trim() : '';
  });
  const revision = Array.from(document.querySelectorAll('code'))
    .map(function (node) { return (node.textContent || '').trim(); })
    .find(function (value) { return value.startsWith('revision '); }) || null;
  const installationRendered =
    text.includes('No installation bound.') || /installation\\s+\\S/.test(text);
  return {
    documentTitle: document.title,
    root: root !== null,
    rootChildren: root ? root.children.length : 0,
    shell: shell,
    heading: heading ? heading.textContent : null,
    text: text,
    rows: rows,
    revision: revision,
    installationRendered: installationRendered,
  };
`;

/** Evaluated in the page on failure. Captures the rendered state for diagnosis. */
const DIAG_STATE = `
  const alertNodes = Array.from(document.querySelectorAll('[role="alert"], .notice, .error'));
  const alertText = alertNodes.length
    ? alertNodes.map(function (node) { return (node.textContent || '').trim(); }).filter(Boolean).join('\\n')
    : (document.body ? (document.body.textContent || '').trim() : '');
  const internals = window.__TAURI_INTERNALS__;
  return {
    documentTitle: document.title,
    locationHref: location.href,
    bodyHtml: document.body ? document.body.innerHTML.slice(0, 2000) : '',
    alertText: alertText.slice(0, 4000),
    tauriInternalsExists: typeof internals !== 'undefined' && internals !== null,
    tauriInvokeIsFunction: !!(internals && typeof internals.invoke === 'function'),
  };
`;

function assert(condition: unknown, message: string): asserts condition {
  if (!condition) throw new Error(message);
}

function sleep(ms: number): Promise<void> {
  return new Promise((done) => setTimeout(done, ms));
}

function describe(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

interface WebDriverResponse {
  sessionId?: string;
}

async function http(
  method: string,
  path: string,
  body?: unknown,
  timeoutMs = 30_000,
): Promise<unknown> {
  const response = await fetch(`${driverUrl}${path}`, {
    method,
    headers: body === undefined ? undefined : { 'content-type': 'application/json' },
    body: body === undefined ? undefined : JSON.stringify(body),
    signal: AbortSignal.timeout(timeoutMs),
  });
  const text = await response.text();
  let payload: { value?: unknown };
  try {
    payload = text === '' ? {} : JSON.parse(text);
  } catch {
    throw new Error(`${method} ${path} returned non-JSON (${response.status}): ${text.slice(0, 500)}`);
  }
  if (!response.ok) {
    throw new Error(`${method} ${path} failed (${response.status}): ${text.slice(0, 1000)}`);
  }
  return payload.value;
}

async function waitForDriver(): Promise<void> {
  const deadline = Date.now() + attachTimeoutMs;
  let last = 'no response';
  while (Date.now() < deadline) {
    try {
      await http('GET', '/status', undefined, 5_000);
      return;
    } catch (error) {
      last = describe(error);
      await sleep(500);
    }
  }
  throw new Error(
    `WebDriver never answered at ${driverUrl} within ${attachTimeoutMs}ms; last error: ${last}`,
  );
}

async function fetchBrowserConsoleMessages(sessionId: string): Promise<unknown[] | null> {
  const attempts: Array<() => Promise<unknown>> = [
    () => http('POST', `/session/${sessionId}/log`, { type: 'browser' }, 10_000),
    () => http('GET', `/session/${sessionId}/log?type=browser`, undefined, 10_000),
  ];
  for (const attempt of attempts) {
    try {
      const value = await attempt();
      if (Array.isArray(value)) return value;
      if (value && typeof value === 'object' && Array.isArray((value as { value?: unknown }).value)) {
        return (value as { value: unknown[] }).value;
      }
      if (value !== null && value !== undefined) return [value];
    } catch {
      // driver may not expose browser logs; try the next shape
    }
  }
  return null;
}

async function waitFor<T>(
  label: string,
  timeoutMs: number,
  probe: () => Promise<T | null>,
): Promise<T> {
  const deadline = Date.now() + timeoutMs;
  let last = 'not ready';
  while (Date.now() < deadline) {
    try {
      const value = await probe();
      if (value !== null) return value;
    } catch (error) {
      last = describe(error);
    }
    await sleep(500);
  }
  throw new Error(`timed out after ${timeoutMs}ms waiting for ${label}; last: ${last}`);
}

let sessionId: string | null = null;
let execute: ((script: string) => Promise<unknown>) | null = null;
let executeAsync: ((script: string) => Promise<unknown>) | null = null;

try {
  assert(
    existsSync(binary),
    `desktop binary is missing at ${binary}; build app/src-tauri before running the smoke lane`,
  );

  await waitForDriver();

  const session = (await http(
    'POST',
    '/session',
    { capabilities: { alwaysMatch: { 'tauri:options': { application: binary } } } },
    attachTimeoutMs,
  )) as WebDriverResponse;
  assert(
    typeof session?.sessionId === 'string' && session.sessionId.length > 0,
    `driver did not return a sessionId: ${JSON.stringify(session)}`,
  );
  sessionId = session.sessionId;

  execute = (script: string): Promise<unknown> =>
    http('POST', `/session/${sessionId}/execute/sync`, { script, args: [] }, 60_000);
  executeAsync = (script: string): Promise<unknown> =>
    http('POST', `/session/${sessionId}/execute/async`, { script, args: [] }, 60_000);

  const nativeTitle = await http('GET', `/session/${sessionId}/title`);
  assert(nativeTitle === 'Marduk', `native window title was ${JSON.stringify(nativeTitle)}`);

  // 1. The shell starts and the bundled frontend mounts.
  const booted = await waitFor('the bundled frontend to mount', uiTimeoutMs, async () => {
    const state = (await execute(DOM_STATE)) as { root: boolean; rootChildren: number };
    return state.root && state.rootChildren > 0 ? state : null;
  });
  const boot = booted as { documentTitle: string; heading: string | null };
  assert(
    boot.documentTitle === 'Marduk',
    `document title was ${JSON.stringify(boot.documentTitle)}`,
  );
  assert(boot.heading?.trim() === 'Marduk', `root heading was ${JSON.stringify(boot.heading)}`);

  // 2. The status view loads from the real workspace, not a fixture. A typed
  //    failure is a failure here: a fresh catalog root must open successfully.
  const statusView = (await waitFor('the authoritative status view', uiTimeoutMs, async () => {
    const state = (await execute(DOM_STATE)) as {
      shell?: boolean;
      heading?: string | null;
      text: string;
      installationRendered?: boolean;
    };
    if (state.shell !== true) return null;
    if (state.heading?.trim() !== 'Marduk') return null;
    if (state.text.includes('Status unavailable')) {
      throw new Error(`the shell rendered its typed failure state: ${state.text.slice(0, 600)}`);
    }
    if (!(state.text.includes('No installation bound.') || state.installationRendered === true)) {
      return null;
    }
    return state.text.includes('Deploy') && state.text.includes('Profile')
      ? state
      : null;
  })) as { text: string; rows: string[] };

  assert(
    statusView.text.includes('No installation bound.'),
    'a fresh catalog root did not render the empty installation state',
  );
  const deployRow = statusView.rows.find((row) => row.startsWith('Deploy'));
  assert(
    deployRow !== undefined &&
      deployRow.includes('Blocked') &&
      deployRow.includes('UnsupportedTarget'),
    `deploy availability was not the typed unavailable state: ${JSON.stringify(deployRow)}`,
  );
  const restoreRow = statusView.rows.find((row) => row.startsWith('Restore'));
  assert(
    restoreRow !== undefined &&
      restoreRow.includes('Blocked') &&
      restoreRow.includes('UnsupportedTarget'),
    `restore availability was not the typed unavailable state: ${JSON.stringify(restoreRow)}`,
  );

  // 3. The command surface exists. This is the same IPC entry point the
  //    bundled transport calls; the rendered view above already proves the
  //    app's envelope parsers accepted the same command on mount.
  const invoked = (await executeAsync(`
    const done = arguments[arguments.length - 1];
    const internals = window.__TAURI_INTERNALS__;
    if (!internals || typeof internals.invoke !== 'function') {
      done({ transport: false, error: '__TAURI_INTERNALS__.invoke is absent' });
      return;
    }
    internals.invoke('status').then(
      function (value) { done({ transport: true, value: value }); },
      function (error) { done({ transport: true, error: String(error) }); },
    );
  `)) as { transport: boolean; value?: unknown; error?: string };

  assert(
    invoked.transport === true,
    `the shell is missing its typed command transport: ${invoked.error ?? 'unknown'}`,
  );
  assert(invoked.error === undefined, `the status command failed: ${invoked.error}`);

  const envelope = invoked.value as {
    ok?: boolean;
    error?: unknown;
    result?: {
      installation?: unknown;
      profile?: { id?: unknown; revision?: unknown; members?: unknown };
      availability?: {
        deploy?: { available?: unknown; blocker?: { code?: unknown } };
        restore?: { available?: unknown };
      };
      pending?: unknown;
    };
  };
  assert(envelope !== null && typeof envelope === 'object', 'status returned no envelope');
  assert(
    envelope.ok === true,
    `status refused a fresh workspace: ${JSON.stringify(envelope.error)}`,
  );
  const result = envelope.result;
  assert(result !== null && typeof result === 'object', 'status envelope had no result');
  assert(result.installation === null, 'fresh app data unexpectedly bound an installation');
  assert(typeof result.profile?.id === 'string' && /^\d+$/.test(result.profile.id), 'profile id was not a decimal string');
  assert(
    typeof result.profile?.revision === 'string' && /^\d+$/.test(result.profile.revision),
    'profile revision was not a decimal string',
  );
  assert(Array.isArray(result.profile?.members), 'profile members were not an array');
  assert(
    result.availability?.deploy?.available === false,
    'status claimed deploy is available; the native effect port is not connected',
  );
  assert(
    result.availability?.deploy?.blocker?.code === 'UnsupportedTarget',
    `deploy blocker was ${JSON.stringify(result.availability?.deploy?.blocker)}`,
  );
  assert(
    result.availability?.restore?.available === false,
    'status claimed restore is available; the native effect port is not connected',
  );
  assert(result.pending === null, 'fresh workspace reported pending work');

  // The rendered revision and the command result must be the same authority.
  const after = (await execute(DOM_STATE)) as { revision: string | null };
  assert(
    after.revision === `revision ${result.profile.revision}`,
    `rendered revision ${JSON.stringify(after.revision)} does not match command revision ${result.profile.revision}`,
  );

  console.log(
    `desktop smoke passed: session ${sessionId}, profile ${result.profile.id} revision ${result.profile.revision}, deploy/restore unavailable`,
  );
} catch (error) {
  console.error(`desktop smoke failed: ${describe(error)}`);
  if (sessionId !== null && execute !== null) {
    try {
      const diag = (await execute(DIAG_STATE)) as {
        documentTitle?: string;
        locationHref?: string;
        bodyHtml?: string;
        alertText?: string;
        tauriInternalsExists?: boolean;
        tauriInvokeIsFunction?: boolean;
      } | null;
      if (diag) {
        console.error(`document title: ${diag.documentTitle ?? '(none)'}`);
        console.error(`location.href: ${diag.locationHref ?? '(none)'}`);
        console.error(`body.innerHTML (first 2000 chars):\n${diag.bodyHtml ?? ''}`);
        console.error(`alert/notice/error text: ${diag.alertText ?? '(none)'}`);
        console.error(
          `window.__TAURI_INTERNALS__ exists: ${diag.tauriInternalsExists === true}; invoke is function: ${diag.tauriInvokeIsFunction === true}`,
        );
      }
    } catch (diagError) {
      console.error(`diagnostic DOM capture failed: ${describe(diagError)}`);
    }
    try {
      const consoleMessages = await fetchBrowserConsoleMessages(sessionId);
      if (consoleMessages !== null) {
        console.error(`captured browser console messages (${consoleMessages.length}):`);
        for (const message of consoleMessages) {
          console.error(typeof message === 'string' ? message : JSON.stringify(message));
        }
      }
    } catch (consoleError) {
      // skip silently if the driver does not expose console messages
    }
  }
  process.exitCode = 1;
} finally {
  if (sessionId !== null) {
    await http('DELETE', `/session/${sessionId}`, undefined, 10_000).catch(() => {});
  }
}

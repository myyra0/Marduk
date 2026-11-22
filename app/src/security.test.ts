// @vitest-environment node

import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';

import { describe, expect, it } from 'vitest';

function read(path: string): string {
  return readFileSync(resolve(process.cwd(), path), 'utf8');
}

function readJson<T>(path: string): T {
  return JSON.parse(read(path)) as T;
}

const FROZEN_CSP =
  "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self'; object-src 'none'; base-uri 'none'; connect-src ipc: http://ipc.localhost";

function matches(source: string, pattern: RegExp, group = 1): string[] {
  return [...source.matchAll(pattern)].map((match) => match[group]);
}

function directives(csp: string): Map<string, string[]> {
  const parsed = new Map<string, string[]>();
  for (const part of csp.split(';')) {
    const trimmed = part.trim();
    if (trimmed.length === 0) continue;
    const [name, ...sources] = trimmed.split(/\s+/);
    parsed.set(name, sources);
  }
  return parsed;
}

const clientSource = read('src/transport/client.ts');
const tsCommands = matches(
  clientSource.match(/export const COMMANDS = \[([\s\S]*?)\] as const/)?.[1] ?? '',
  /'([a-z_]+)'/g,
).sort();

const buildSource = read('src-tauri/build.rs');
const manifestCommands = matches(
  buildSource.match(/commands\(&\[([\s\S]*?)\]\)/)?.[1] ?? '',
  /"([a-z_]+)"/g,
).sort();

const commandsSource = read('src-tauri/src/commands.rs');
const rustCommands = matches(
  commandsSource,
  /#\[tauri::command(?:\([^)]*\))?\]\s*pub\s+(?:async\s+)?fn\s+([a-z_]+)/g,
).sort();

const libSource = read('src-tauri/src/lib.rs');
const handlerCommands = matches(
  libSource.match(/generate_handler!\[([\s\S]*?)\]\)/)?.[1] ?? '',
  /commands::([a-z_]+)/g,
).sort();

const capability = readJson<{
  windows: string[];
  permissions: string[];
}>( 'src-tauri/capabilities/main.json');

const capabilityCommands = capability.permissions
  .filter((permission) => permission.startsWith('allow-'))
  .map((permission) => permission.slice('allow-'.length).replaceAll('-', '_'))
  .sort();

const config = readJson<{
  identifier: string;
  app: {
    windows: Array<{ label: string; url?: string }>;
    security: { csp: string; devCsp: string };
    withGlobalTauri?: boolean;
  };
}>( 'src-tauri/tauri.conf.json');

describe('desktop capability surface', () => {
  it('exposes exactly the frozen command set everywhere', () => {
    expect(tsCommands).toEqual([
      'deploy_apply',
      'deploy_preview',
      'import_apply',
      'import_preview',
      'operation',
      'order',
      'pick_installation',
      'pick_zip',
      'restore',
      'setup',
      'status',
    ]);
    expect(manifestCommands).toEqual(tsCommands);
    expect(rustCommands).toEqual(tsCommands);
    expect(handlerCommands).toEqual(tsCommands);
    expect(capabilityCommands).toEqual(tsCommands);
  });

  it('scopes the capability to the one local window', () => {
    expect(capability.windows).toEqual(['main']);
    expect(config.app.windows.map((window) => window.label)).toEqual(['main']);
    expect(config.app.windows.every((window) => window.url === undefined)).toBe(true);
  });

  it('grants no filesystem, shell, network or updater permission', () => {
    for (const permission of capability.permissions) {
      expect(permission).not.toMatch(/(^|:)(fs|shell|http|updater|process)(:|$)/);
    }
    const cargo = read('src-tauri/Cargo.toml');
    expect(cargo).not.toMatch(
      /tauri-plugin-(fs|shell|http|updater|process|single-instance|opener)/,
    );
    expect(cargo).toMatch(/tauri-plugin-dialog\s*=\s*"2"/);
    expect(libSource).not.toMatch(/tauri_plugin_(fs|shell|http|updater|process|opener)/);
  });

  it('uses the frozen CSP and bundle identifier', () => {
    expect(config.identifier).toBe('com.marduk.desktop');
    expect(config.app.security.csp).toBe(FROZEN_CSP);
    expect(config.app.withGlobalTauri).toBeUndefined();
  });

  it('keeps the dev CSP to the frozen policy plus dev origins only', () => {
    const dev = config.app.security.devCsp;
    expect(dev).not.toMatch(/unsafe-inline|unsafe-eval|\*/);

    const frozen = directives(FROZEN_CSP);
    const development = directives(dev);
    expect([...development.keys()]).toEqual([...frozen.keys()]);

    const allowedExtras = new Set([
      'http://localhost:1420',
      'ws://localhost:1420',
      "'nonce-marduk-dev'",
    ]);
    for (const [name, sources] of frozen) {
      const devSources = development.get(name) ?? [];
      for (const source of sources) {
        expect(devSources, `${name} ${source}`).toContain(source);
      }
      for (const source of devSources) {
        if (!sources.includes(source)) {
          expect(allowedExtras, `${name} ${source}`).toContain(source);
        }
      }
    }
    expect(dev).toContain('http://localhost:1420');
    expect(dev).toContain('ws://localhost:1420');
  });

  it('carries ordered progress only through per-write typed channels', () => {
    expect(commandsSource.match(/on_progress\s*:\s*Channel\s*<\s*Progress\s*>/g)).toHaveLength(2);
    expect(commandsSource).toMatch(/pub fn deploy_apply/);
    expect(commandsSource).toMatch(/pub fn restore/);
    for (const readOnly of ['deploy_preview', 'status']) {
      const signature = commandsSource.match(new RegExp(`pub fn ${readOnly}\\(([\\s\\S]*?)\\)`));
      expect(signature?.[1] ?? '').not.toMatch(/Channel/);
    }
    expect(clientSource).toMatch(/progressChannel/);
  });

  it('keeps Rust commands free of generic write or process authority', () => {
    expect(commandsSource).not.toMatch(/std::fs::|std::process|Command::new|OpenOptions/);
    // The desktop commands carry opaque handles only; paths stay in Rust.
    expect(commandsSource).not.toMatch(/"gameDir"\s*:|"zip"\s*:/);
    expect(commandsSource).toMatch(/"selectionId"/);
    expect(clientSource).not.toMatch(/(^|\n)\s*export\s+(async\s+)?function\s+(invoke|send|call)\b/);
    expect(clientSource).not.toMatch(/@tauri-apps\/plugin-(fs|shell|http|dialog)/);
  });

  it('never lets product modules import test fixtures', () => {
    for (const file of ['src/App.tsx', 'src/main.tsx', 'src/transport/client.ts', 'src/transport/wire.ts']) {
      expect(read(file), file).not.toMatch(/fixtures\/|fixtureTransport|test\/setup/);
    }
  });
});

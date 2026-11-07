// Shika browser prototype: sample data only. No agent, Git command, or network
// request runs; every terminal line below is written here. The layout and
// behavior follow design/DESIGN.md and the native app's keyboard flow.
(function () {
  'use strict';

  // Icons, copied from the app (crates/shika/src/ui.rs, agent_marks.rs, lead_chat).
  var ICON = {
    column: '<svg viewBox="0 0 16 16"><path fill="currentColor" fill-rule="evenodd" d="M3 2h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Zm3 1.5H3a.5.5 0 0 0-.5.5v8a.5.5 0 0 0 .5.5h3v-9Zm1.5 0v9H13a.5.5 0 0 0 .5-.5V4a.5.5 0 0 0-.5-.5H7.5Z"/></svg>',
    changes: '<svg viewBox="0 0 16 16"><path transform="matrix(-1 0 0 1 16 0)" fill="currentColor" fill-rule="evenodd" d="M3 2h10a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H3a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2Zm3 1.5H3a.5.5 0 0 0-.5.5v8a.5.5 0 0 0 .5.5h3v-9Zm1.5 0v9H13a.5.5 0 0 0 .5-.5V4a.5.5 0 0 0-.5-.5H7.5Z"/></svg>',
    branch: '<svg viewBox="0 0 16 16"><path fill="currentColor" fill-rule="evenodd" d="M9.5 3.25a2.25 2.25 0 1 1 3 2.122V6A2.5 2.5 0 0 1 10 8.5H6a1 1 0 0 0-1 1v1.128a2.251 2.251 0 1 1-1.5 0V5.372a2.25 2.25 0 1 1 1.5 0v1.836A2.493 2.493 0 0 1 6 7h4a1 1 0 0 0 1-1v-.628A2.25 2.25 0 0 1 9.5 3.25Zm-6 0a.75.75 0 1 0 1.5 0 .75.75 0 0 0-1.5 0Zm8.25-.75a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5ZM4.25 12a.75.75 0 1 0 0 1.5.75.75 0 0 0 0-1.5Z"/></svg>',
    folderPlus: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M12 10.5v6m3-3H9m4.06-7.19-2.12-2.12a1.5 1.5 0 0 0-1.061-.44H4.5A2.25 2.25 0 0 0 2.25 6v12a2.25 2.25 0 0 0 2.25 2.25h15A2.25 2.25 0 0 0 21.75 18V9a2.25 2.25 0 0 0-2.25-2.25h-5.379a1.5 1.5 0 0 1-1.06-.44Z"/></svg>',
    plus: '<svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"><path d="M8 3v10M3 8h10"/></svg>',
    gear: '<svg viewBox="0 0 24 24"><path fill="currentColor" fill-rule="evenodd" d="M11.078 2.25c-.917 0-1.699.663-1.85 1.567L9.05 4.889c-.02.12-.115.26-.297.348a7.493 7.493 0 0 0-.986.57c-.166.115-.334.126-.45.083L6.3 5.508a1.875 1.875 0 0 0-2.282.819l-.922 1.597a1.875 1.875 0 0 0 .432 2.385l.84.692c.095.078.17.229.154.43a7.598 7.598 0 0 0 0 1.139c.015.2-.059.352-.153.43l-.841.692a1.875 1.875 0 0 0-.432 2.385l.922 1.597a1.875 1.875 0 0 0 2.282.818l1.019-.382c.115-.043.283-.031.45.082.312.214.641.405.985.57.182.088.277.228.297.35l.178 1.071c.151.904.933 1.567 1.85 1.567h1.844c.916 0 1.699-.663 1.85-1.567l.178-1.072c.02-.12.114-.26.297-.349.344-.165.673-.356.985-.57.167-.114.335-.125.45-.082l1.02.382a1.875 1.875 0 0 0 2.28-.819l.923-1.597a1.875 1.875 0 0 0-.432-2.385l-.84-.692c-.095-.078-.17-.229-.154-.43a7.614 7.614 0 0 0 0-1.139c-.016-.2.059-.352.153-.43l.84-.692c.708-.582.891-1.59.433-2.385l-.922-1.597a1.875 1.875 0 0 0-2.282-.818l-1.02.382c-.114.043-.282.031-.449-.083a7.49 7.49 0 0 0-.985-.57c-.183-.087-.277-.227-.297-.348l-.179-1.072a1.875 1.875 0 0 0-1.85-1.567h-1.843ZM12 15.75a3.75 3.75 0 1 0 0-7.5 3.75 3.75 0 0 0 0 7.5Z"/></svg>',
    antlers: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"><path d="M10.2 15.2C6.6 14.6 4.4 11.4 4.6 4"/><path d="M5.1 11C4.2 10.4 3.4 9.4 3 8"/><path d="M8.6 13.4C8 11.4 8.4 8.6 9.4 6.6"/><path d="M13.8 15.2C17.4 14.6 19.6 11.4 19.4 4"/><path d="M18.9 11C19.8 10.4 20.6 9.4 21 8"/><path d="M15.4 13.4C16 11.4 15.6 8.6 14.6 6.6"/><path fill="currentColor" d="M10 16.4 12 20.2 14 16.4Z"/></svg>',
    send: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"><path d="M12 19V5"/><path d="M5.5 11.5 12 5l6.5 6.5"/></svg>'
  };
  // Lobe Icons (MIT) and Pi (MIT) marks, as in agent_marks.rs. The names and
  // marks are their owners' trademarks.
  var MARK = {
    claude: '<svg viewBox="0 0 24 24"><path fill-rule="evenodd" d="M4.709 15.955l4.72-2.647.08-.23-.08-.128H9.2l-.79-.048-2.698-.073-2.339-.097-2.266-.122-.571-.121L0 11.784l.055-.352.48-.321.686.06 1.52.103 2.278.158 1.652.097 2.449.255h.389l.055-.157-.134-.098-.103-.097-2.358-1.596-2.552-1.688-1.336-.972-.724-.491-.364-.462-.158-1.008.656-.722.881.06.225.061.893.686 1.908 1.476 2.491 1.833.365.304.145-.103.019-.073-.164-.274-1.355-2.446-1.446-2.49-.644-1.032-.17-.619a2.97 2.97 0 01-.104-.729L6.283.134 6.696 0l.996.134.42.364.62 1.414 1.002 2.229 1.555 3.03.456.898.243.832.091.255h.158V9.01l.128-1.706.237-2.095.23-2.695.08-.76.376-.91.747-.492.584.28.48.685-.067.444-.286 1.851-.559 2.903-.364 1.942h.212l.243-.242.985-1.306 1.652-2.064.73-.82.85-.904.547-.431h1.033l.76 1.129-.34 1.166-1.064 1.347-.881 1.142-1.264 1.7-.79 1.36.073.11.188-.02 2.856-.606 1.543-.28 1.841-.315.833.388.091.395-.328.807-1.969.486-2.309.462-3.439.813-.042.03.049.061 1.549.146.662.036h1.622l3.02.225.79.522.474.638-.079.485-1.215.62-1.64-.389-3.829-.91-1.312-.329h-.182v.11l1.093 1.068 2.006 1.81 2.509 2.33.127.578-.322.455-.34-.049-2.205-1.657-.851-.747-1.926-1.62h-.128v.17l.444.649 2.345 3.521.122 1.08-.17.353-.608.213-.668-.122-1.374-1.925-1.415-2.167-1.143-1.943-.14.08-.674 7.254-.316.37-.729.28-.607-.461-.322-.747.322-1.476.389-1.924.315-1.53.286-1.9.17-.632-.012-.042-.14.018-1.434 1.967-2.18 2.945-1.726 1.845-.414.164-.717-.37.067-.662.401-.589 2.388-3.036 1.44-1.882.93-1.086-.006-.158h-.055L4.132 18.56l-1.13.146-.487-.456.061-.746.231-.243 1.908-1.312-.006.006z"/></svg>',
    codex: '<svg viewBox="0 0 24 24"><path fill-rule="evenodd" d="M8.086.457a6.105 6.105 0 013.046-.415c1.333.153 2.521.72 3.564 1.7a.117.117 0 00.107.029c1.408-.346 2.762-.224 4.061.366l.063.03.154.076c1.357.703 2.33 1.77 2.918 3.198.278.679.418 1.388.421 2.126a5.655 5.655 0 01-.18 1.631.167.167 0 00.04.155 5.982 5.982 0 011.578 2.891c.385 1.901-.01 3.615-1.183 5.14l-.182.22a6.063 6.063 0 01-2.934 1.851.162.162 0 00-.108.102c-.255.736-.511 1.364-.987 1.992-1.199 1.582-2.962 2.462-4.948 2.451-1.583-.008-2.986-.587-4.21-1.736a.145.145 0 00-.14-.032c-.518.167-1.04.191-1.604.185a5.924 5.924 0 01-2.595-.622 6.058 6.058 0 01-2.146-1.781c-.203-.269-.404-.522-.551-.821a7.74 7.74 0 01-.495-1.283 6.11 6.11 0 01-.017-3.064.166.166 0 00.008-.074.115.115 0 00-.037-.064 5.958 5.958 0 01-1.38-2.202 5.196 5.196 0 01-.333-1.589 6.915 6.915 0 01.188-2.132c.45-1.484 1.309-2.648 2.577-3.493.282-.188.55-.334.802-.438.286-.12.573-.22.861-.304a.129.129 0 00.087-.087A6.016 6.016 0 015.635 2.31C6.315 1.464 7.132.846 8.086.457zm-.804 7.85a.848.848 0 00-1.473.842l1.694 2.965-1.688 2.848a.849.849 0 001.46.864l1.94-3.272a.849.849 0 00.007-.854l-1.94-3.393zm5.446 6.24a.849.849 0 000 1.695h4.848a.849.849 0 000-1.696h-4.848z"/></svg>',
    cursor: '<svg viewBox="0 0 24 24"><path fill-rule="evenodd" d="M22.106 5.68L12.5.135a.998.998 0 00-.998 0L1.893 5.68a.84.84 0 00-.419.726v11.186c0 .3.16.577.42.727l9.607 5.547a.999.999 0 00.998 0l9.608-5.547a.84.84 0 00.42-.727V6.407a.84.84 0 00-.42-.726zm-.603 1.176L12.228 22.92c-.063.108-.228.064-.228-.061V12.34a.59.59 0 00-.295-.51l-9.11-5.26c-.107-.062-.063-.228.062-.228h18.55c.264 0 .428.286.296.514z"/></svg>',
    pi: '<svg viewBox="-1 -1 18 18"><path fill-rule="evenodd" d="M0 0h12v4H0zM0 4h4v12H0zM8 4h4v4H8zM4 8h4v4H4zM12 8h4v8h-4z"/></svg>'
  };

  var CLIS = [
    { id: 'claude', name: 'Claude Code', path: '/opt/homebrew/bin/claude' },
    { id: 'codex', name: 'Codex', path: '~/.local/bin/codex' },
    { id: 'cursor', name: 'Cursor CLI', path: '~/.local/bin/agent' },
    { id: 'pi', name: 'Pi', path: '/opt/homebrew/bin/pi' }
  ];
  var cliOf = function (id) { return CLIS.filter(function (c) { return c.id === id; })[0]; };
  var RANK = { asking: 0, ready: 1, working: 2, waiting: 3, terminal: 4 };

  var params = new URLSearchParams(location.search);
  var root = document.documentElement;
  var embedded = params.get('embed') === '1';

  // ---------------------------------------------------------------- helpers
  function esc(s) { return String(s).replace(/[&<>"]/g, function (c) { return { '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;' }[c]; }); }
  // A terminal line is a list of [text, class] segments.
  function seg(t, c) { return [t, c || '']; }
  function line() { return Array.prototype.slice.call(arguments).map(function (x) { return typeof x === 'string' ? seg(x) : x; }); }
  function blank() { return []; }
  function shorten(t, n) {
    t = t.replace(/\s+/g, ' ').trim();
    if (t.length <= n) return t;
    var cut = t.slice(0, n), i = cut.lastIndexOf(' ');
    return (i > n / 2 ? cut.slice(0, i) : cut).replace(/[,.;:]$/, '') + '…';
  }
  function slug(t) {
    var stop = ['the', 'a', 'an', 'to', 'so', 'and', 'of', 'in', 'for', 'it', 'is', 'with', 'on'];
    return t.toLowerCase().replace(/[^a-z0-9\s-]/g, '').split(/\s+/).filter(function (w) { return w && stop.indexOf(w) < 0; }).slice(0, 4).join('-').slice(0, 40) || 'task';
  }
  function elapsed(ms) {
    var s = Math.max(0, Math.floor(ms / 1000));
    if (s < 60) return s + 's';
    var m = Math.floor(s / 60);
    return m < 60 ? m + 'm' : Math.floor(m / 60) + 'h ' + (m % 60) + 'm';
  }
  function plural(n, w) { return n + ' ' + w + (n === 1 ? '' : 's'); }
  function sum(files, k) { return files.reduce(function (a, f) { return a + f[k]; }, 0); }
  var seq = 100;
  function nextId(p) { seq += 1; return p + seq; }
  function hex() { return Math.random().toString(16).slice(2, 9); }
  function now() { return Date.now(); }

  // ------------------------------------------------------------- CLI output
  // Each CLI's own look, simplified. Shika paints whatever the CLI prints.
  var CLI = {
    claude: {
      banner: function (c) { return [line(seg('✻ ', 'c-salmon'), seg('Claude Code', 'c-white b')), line(seg('  ' + c.path, 'c-dim')), blank()]; },
      prompt: function (t) { return [line(seg('❯ ', 'c-dim'), seg(t, 'c-white')), blank()]; },
      tool: function (name, arg, result) {
        var out = [line(seg('⏺ ', 'c-green'), seg(name, 'b'), '(' + arg + ')')];
        if (result) out.push(line(seg('  ⎿  ', 'c-dim'), seg(result, 'c-dim')));
        return out;
      },
      diff: function (rows) { return rows.map(function (r) { return line(seg('      ' + r[0] + ' ', 'c-dim'), seg((r[1] === '+' ? '+' : r[1] === '-' ? '-' : ' ') + ' ' + r[2], r[1] === '+' ? 'bg-add' : r[1] === '-' ? 'bg-del' : '')); }); },
      say: function (t) { return [line(seg('⏺ ', 'c-white'), t), blank()]; },
      working: function (ms) { return [line(seg('✻ ', 'c-salmon'), seg('Working… ', 'c-salmon'), seg('(' + elapsed(ms) + ' · esc to interrupt)', 'c-dim'))]; },
      inputBox: true,
      footer: [seg('  ⏵⏵ bypass permissions on', 'c-magenta'), seg(' (shift+tab to cycle)', 'c-dim')]
    },
    codex: {
      banner: function (c) { return [line(seg('>_ ', 'c-dim'), seg('Codex', 'c-white b')), line(seg('   directory: ', 'c-dim'), seg(c.path, 'c-dim')), line(seg('   approval: never · sandbox: off', 'c-dim')), blank()]; },
      prompt: function (t) { return [line(seg('› ', 'c-dim'), seg(t, 'c-white')), blank()]; },
      tool: function (name, arg, result) {
        var out = [line(seg('• ', 'c-green'), seg(name, 'b'), ' ' + arg)];
        if (result) out.push(line(seg('  └ ', 'c-dim'), seg(result, 'c-dim')));
        return out;
      },
      diff: function (rows) { return rows.map(function (r) { return line(seg('    ' + r[0] + ' ', 'c-dim'), seg((r[1] === ' ' ? ' ' : r[1]) + r[2], r[1] === '+' ? 'c-green' : r[1] === '-' ? 'c-red' : 'c-dim')); }); },
      say: function (t) { return [line(seg('─ Worked for 2m 41s ', 'c-faint'), seg('─'.repeat(40), 'c-faint')), blank(), line(t), blank()]; },
      working: function (ms) { return [line(seg('• ', 'c-dim'), seg('Working ', 'b'), seg('(' + elapsed(ms) + ' • esc to interrupt)', 'c-dim'))]; },
      inputPrefix: '› ',
      footer: [seg('  ⏎ send   ⌃J newline   ⌃T transcript', 'c-dim')]
    },
    cursor: {
      banner: function (c) { return [line(seg('Cursor Agent', 'c-white b')), line(seg(c.path, 'c-dim')), blank()]; },
      prompt: function (t) { return [line(seg('→ ', 'c-dim'), seg(t, 'c-white')), blank()]; },
      tool: function (name, arg, result) {
        var out = [line(seg('⬢ ', 'c-cyan'), seg(name + ' ', 'c-dim'), arg)];
        if (result) out.push(line(seg('  ' + result, 'c-dim')));
        return out;
      },
      diff: function (rows) { return rows.map(function (r) { return line(seg('  ' + r[0] + ' ', 'c-dim'), seg((r[1] === ' ' ? ' ' : r[1]) + r[2], r[1] === '+' ? 'c-green' : r[1] === '-' ? 'c-red' : 'c-dim')); }); },
      say: function (t) { return [blank(), line(t), blank()]; },
      working: function (ms) { var f = '⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏'; return [line(seg(f[Math.floor(ms / 120) % f.length] + ' ', 'c-cyan'), seg('Generating', 'c-dim'), seg('  ' + elapsed(ms), 'c-faint'))]; },
      inputPrefix: '→ ',
      footer: [seg('  Auto-run all · /help for commands', 'c-dim')]
    },
    pi: {
      banner: function (c) { return [line(seg('pi', 'c-white b'), seg('  ' + c.path, 'c-dim')), blank()]; },
      prompt: function (t) { return [line(seg('> ', 'c-blue'), seg(t, 'c-white')), blank()]; },
      tool: function (name, arg, result) {
        var out = [line(seg(name.toLowerCase() + ' ', 'c-yellow'), arg)];
        if (result) out.push(line(seg('  ' + result, 'c-dim')));
        return out;
      },
      diff: function (rows) { return rows.map(function (r) { return line(seg('  ' + (r[1] === ' ' ? ' ' : r[1]) + r[2], r[1] === '+' ? 'c-green' : r[1] === '-' ? 'c-red' : 'c-dim')); }); },
      say: function (t) { return [blank(), line(t), blank()]; },
      working: function (ms) { return [line(seg('● ', 'c-blue'), seg('working… ' + elapsed(ms), 'c-dim'))]; },
      inputPrefix: '> ',
      footer: [seg('  ~/code · approve on', 'c-dim')]
    }
  };

  // ------------------------------------------------------------ sample data
  // The diffs a finished turn leaves. [line number, '+', '-' or ' ', text].
  var FILES = {
    webhooks: [
      { path: 'src/webhooks/stripe.js', status: 'Modified', add: 30, del: 7, rows: [
        [1, ' ', 'import { markOrderPaid, markOrderFailed } from "../orders/orders.js";'],
        [3, '+', 'const MAX_RETRIES = 5;'],
        [4, '+', 'const BASE_DELAY_MS = 100;'],
        [6, '+', '// Events that still failed after every retry, kept for a person to look at.'],
        [7, '+', 'export const deadLetters = [];'],
        [15, '-', '// Called by the HTTP layer. A failure is logged and the event is dropped.'],
        [16, '-', 'export async function receive(event, store, log = console) {'],
        [23, '+', '// Called by the HTTP layer. A failure is retried with exponential backoff'],
        [24, '+', '// (100ms, 200ms, 400ms, ...). Once retries run out the event goes to the'],
        [25, '+', '// dead letter list.'],
        [26, '+', 'export async function receive('],
        [27, '+', '  event, store, log = console,'],
        [28, '+', '  { retries = MAX_RETRIES, baseDelayMs = BASE_DELAY_MS, sleep = wait } = {},'],
        [29, '+', ') {'],
        [30, '+', '  for (let attempt = 0; ; attempt++) {']
      ] },
      { path: 'test/stripe.test.js', status: 'Untracked', add: 77, del: 0, rows: [
        [1, '+', 'import { test } from "node:test";'],
        [2, '+', 'import assert from "node:assert/strict";'],
        [3, '+', 'import { receive, deadLetters } from "../src/webhooks/stripe.js";'],
        [5, '+', 'test("retries with backoff, then succeeds", async () => {'],
        [6, '+', '  const waits = [];'],
        [7, '+', '  const store = flakyStore({ failures: 3 });'],
        [8, '+', '  await receive(paid, store, quiet, { sleep: async (ms) => waits.push(ms) });'],
        [9, '+', '  assert.deepEqual(waits, [100, 200, 400]);']
      ] }
    ],
    tax: [
      { path: 'src/checkout/tax.js', status: 'Untracked', add: 64, del: 0, rows: [
        [1, '+', '// Sales tax in cents, from the shipping address.'],
        [2, '+', 'export function salesTax(cart, address, rates = STATE_RATES) {'],
        [3, '+', '  const rate = rates[address.state] ?? 0;'],
        [4, '+', '  const taxable = cart.items.filter((item) => !item.exempt);'],
        [5, '+', '  return Math.round(subtotal(taxable) * rate);'],
        [6, '+', '}']
      ] },
      { path: 'src/checkout/total.js', status: 'Modified', add: 6, del: 2, rows: [
        [1, '-', '// Order total in cents. Tax is not applied yet.'],
        [1, '+', '// Order total in cents, with sales tax from the shipping address.'],
        [6, '-', '  return { subtotal, shipping, tax: 0, total: subtotal + shipping };'],
        [6, '+', '  const tax = salesTax(cart, address);'],
        [7, '+', '  return { subtotal, shipping, tax, total: subtotal + shipping + tax };']
      ] }
    ],
    routes: [
      { path: 'src/router.rs', status: 'Modified', add: 32, del: 0, rows: [
        [52, '+', '    /// Every route in the order it was added, as method, path, and handler.'],
        [53, '+', '    pub fn routes(&self) -> impl Iterator<Item = (Method, String, &str)> + \'_ {'],
        [54, '+', '        self.routes.iter().map(|(method, segments, handler)| {'],
        [55, '+', '            (*method, format!("/{}", segments.join("/")), handler.as_str())'],
        [56, '+', '        })'],
        [57, '+', '    }']
      ] },
      { path: 'tests/router.rs', status: 'Modified', add: 43, del: 0, rows: [
        [12, '+', '#[test]'],
        [13, '+', 'fn display_prints_one_route_per_line() {'],
        [14, '+', '    let mut router = Router::new();'],
        [15, '+', '    router.add(Method::Get, "/users/:id", "show_user");'],
        [16, '+', '    assert_eq!(router.to_string(), "GET /users/:id -> show_user\\n");']
      ] },
      { path: 'Cargo.lock', status: 'Untracked', add: 7, del: 0, rows: [
        [1, '+', '# This file is automatically @generated by Cargo.'],
        [2, '+', '# It is not intended for manual editing.'],
        [3, '+', 'version = 4']
      ] }
    ],
    slash: [
      { path: 'src/router.rs', status: 'Modified', add: 9, del: 2, rows: [
        [38, '-', '        let parts: Vec<&str> = path.trim_matches(\'/\').split(\'/\').collect();'],
        [38, '+', '        // "/users/" and "/users" are the same route.'],
        [39, '+', '        let parts: Vec<&str> = path.trim_matches(\'/\').split(\'/\').collect();'],
        [40, '+', '        let parts = if parts == [""] { vec![] } else { parts };']
      ] }
    ]
  };

  function makeCard(o) {
    var c = Object.assign({
      title: null, named: false, status: 'waiting', seen: true, startedAt: now(), finishAt: null,
      lines: [], input: '', ask: null, files: [], committed: 0, pushed: 0, staged: false,
      tabs: [], tab: 0, nextShell: 1, pr: null, worker: false, closing: false, script: null
    }, o);
    c.path = '~/code/' + c.project + '/.worktrees/' + (c.dir || c.branch);
    c.lines = CLI[c.cli].banner(c).concat(c.lines || []);
    return c;
  }

  function sampleState() {
    var t = now();
    var cards = {};
    var add = function (c) { cards[c.id] = c; return c.id; };
    var C = CLI;
    var b = function (t) { return seg(t, 'c-white b'); };
    var code = function (t) { return seg(t, 'c-blue'); };

    add(makeCard({
      id: 'a1', project: 'storefront', cli: 'claude', title: 'Stripe webhook retry with dead letter', named: true,
      branch: 'stripe-webhook-retry-with-dead-letter', dir: 'shika-draft-18e5a3fe', status: 'ready', seen: false, files: FILES.webhooks,
      lines: [].concat(
        C.claude.prompt('Retry failed Stripe webhooks with backoff instead of dropping them. In src/webhooks/stripe.js, retry up to five times with exponential backoff, then keep the event in a dead letter list. Add a test and run npm test.'),
        C.claude.tool('Read', 'src/webhooks/stripe.js', 'Read 24 lines'),
        C.claude.tool('Update', 'src/webhooks/stripe.js', 'Added 30 lines, removed 7 lines'),
        C.claude.tool('Write', 'test/stripe.test.js', 'Wrote 77 lines to test/stripe.test.js'),
        C.claude.tool('Bash', 'npm test', '✔ 4 passed'),
        [blank()],
        [line(b('How it works now ('), seg('src/webhooks/stripe.js', 'c-blue b'), b(')'))],
        [line('- If handling an event throws, ', code('receive'), ' tries again up to 5 more times, so 6 attempts in total.')],
        [line('  It waits 100ms, 200ms, 400ms, 800ms, then 1600ms between tries.')],
        [line('- If the last try still fails, the event goes into a new ', code('deadLetters'), ' list (exported) as')],
        [line('  ', code('{ event, error, attempts, failedAt }'), '. It logs and returns ', code('null'), ' as before.')],
        [line('- The function still takes ', code('(event, store, log)'), ', so existing callers don\'t change.')],
        [blank()],
        [line(b('Tests ('), seg('test/stripe.test.js', 'c-blue b'), b(')'))],
        [line('1. The store fails 3 times, then the event succeeds with waits of ', code('[100, 200, 400]'), '.')],
        [line('2. The store always fails, so the event is tried 6 times, then lands in the dead letter list.')],
        [line('3. A successful event isn\'t retried or dead-lettered.')],
        [blank()],
        [line(b('Things you may want to change'))],
        [line('- ', b('The dead-letter list is lost on restart.'), ' It\'s kept in memory. For production it should')],
        [line('  be saved to a database or a queue.')],
        [line('- ', b('Some failures are retried pointlessly.'), ' Every error is retried, including ones that will')],
        [line('  never succeed, like "order not found". I can skip retries for those if you\'d like.')],
        [blank()],
        [line(seg('✻ Worked for 29s', 'c-dim'))]
      )
    }));
    add(makeCard({
      id: 'a2', project: 'storefront', cli: 'codex', title: 'Calculate checkout sales tax', named: true,
      branch: 'calculate-checkout-sales-tax', dir: 'shika-draft-18e5a420', status: 'working', startedAt: t - 23000, finishAt: t + 26000,
      script: 'tax',
      lines: [].concat(
        C.codex.prompt('Calculate sales tax at checkout from the shipping address and show it as its own line. Add src/checkout/tax.js with rates for every US state, use it in orderTotal, cover exempt items and unknown states with tests, and run npm test.'),
        C.codex.tool('Explored', 'src/checkout', 'Read total.js, total.test.js'),
        C.codex.tool('Added', 'src/checkout/tax.js (+64 -0)')
      )
    }));
    add(makeCard({
      id: 'a3', project: 'routekit', cli: 'claude', title: 'Router::find trailing slash handling', named: true,
      branch: 'router-find-trailing-slash-handling', dir: 'shika-draft-18e5a408', status: 'asking', seen: true,
      ask: { q: 'Should /users/ match the /users route?', options: ['Match it: treat /users/ the same as /users', 'Redirect /users/ to /users', 'No match: keep paths exact'], choice: 0 },
      lines: [].concat(
        C.claude.prompt('Make Router::find handle trailing slashes. Before changing any code, ask me whether /users/ should match /users, redirect, or return no match.'),
        C.claude.tool('Read', 'src/router.rs', 'Read 52 lines'),
        C.claude.tool('Read', 'tests/router.rs', 'Read 11 lines')
      )
    }));
    add(makeCard({ id: 'a4', project: 'routekit', cli: 'pi', branch: 'shika-draft-18e5a40e', status: 'waiting' }));
    add(makeCard({
      id: 'w1', project: 'routekit', cli: 'cursor', title: 'Router Iterator Display', named: true, worker: true,
      branch: 'router-iterator-display', dir: 'shika-draft-18e5a41b', status: 'ready', seen: false, files: FILES.routes,
      lines: [].concat(
        C.cursor.prompt('Add Router::routes(), an iterator over every route as method, path, and handler, plus a Display impl that prints one route per line. Add tests and run cargo test. Report with shika report when done.'),
        C.cursor.tool('Read', 'src/router.rs'),
        C.cursor.tool('Edited', 'src/router.rs', '+32'),
        C.cursor.tool('Edited', 'tests/router.rs', '+43'),
        C.cursor.tool('Ran', 'cargo test', 'test result: ok. 4 passed; 0 failed'),
        C.cursor.say('Router::routes() and Display are in. Reported to the Lead.')
      )
    }));

    return {
      cards: cards,
      projects: [
        { id: 'storefront', name: 'storefront', base: 'main', cards: ['a1', 'a2'] },
        { id: 'routekit', name: 'routekit', base: 'main', cards: ['a3', 'a4'] },
        { id: 'atlas', name: 'atlas', base: 'main', cards: [] },
        { id: 'field-notes', name: 'field-notes', base: 'main', cards: [] }
      ],
      lead: {
        id: 'lead', project: 'routekit', cli: 'claude', status: 'ready', seen: false, startedAt: t, workers: ['w1'], input: '',
        chat: [
          { who: 'me', text: 'Start one Cursor CLI worker to add Router::routes(), an iterator over every route as method, path, and handler, plus a Display impl that prints one route per line. It should add tests and run cargo test. Tell me when it reports back.' },
          { who: 'steps', text: ['Started Router Iterator Display', 'Waited for workers'] },
          { who: 'lead', text: 'The Cursor worker has reported back, and `Router::routes()` and the `Display` impl are done. `routes()` returns each route\'s method, path, and handler in the order they were added, and `Display` prints one route per line, like `GET /users/:id -> show_user`. It says `cargo test` passed (4 passed, 0 failed). Changes: `src/router.rs` (+32) and `tests/router.rs` (+43) on `router-iterator-display`. Nothing is pushed yet. One thing before publishing: `cargo test` created a `Cargo.lock` that is not ignored, so Create PR would include it.' }
        ]
      },
      sel: 'a1', focus: 'cards', colHidden: false, changesOpen: false,
      overlay: null, toast: null, notes: [], selectedAt: t
    };
  }

  var S = sampleState();

  // ------------------------------------------------------------ navigation
  function project(id) { return S.projects.filter(function (p) { return p.id === id; })[0]; }
  function card(id) { return id === 'lead' ? S.lead : S.cards[id]; }
  function sorted(ids) {
    return ids.map(card).filter(Boolean).sort(function (a, b) { return RANK[a.status] - RANK[b.status]; });
  }
  // Every stop in column order: the Lead and its workers, then project cards.
  function order() {
    var ids = ['lead'].concat(sorted(S.lead.workers).map(function (c) { return c.id; }));
    var groups = S.projects.slice().sort(function (a, b) { return (b.cards.length ? 1 : 0) - (a.cards.length ? 1 : 0); });
    groups.forEach(function (p) { sorted(p.cards).forEach(function (c) { ids.push(c.id); }); });
    return ids;
  }
  function selected() { return S.sel ? card(S.sel) : null; }
  function select(id, focus) {
    if (S.sel !== id) S.selectedAt = now();
    S.sel = id;
    if (focus) S.focus = focus;
    render();
  }
  function move(d, wrap) {
    var ids = order(), i = ids.indexOf(S.sel), n;
    if (!ids.length) return;
    if (i < 0) n = d > 0 ? 0 : ids.length - 1;
    else n = wrap ? (i + d + ids.length) % ids.length : Math.max(0, Math.min(ids.length - 1, i + d));
    select(ids[n]);
  }
  function currentProject() {
    var c = selected();
    return c ? c.project : S.projects[0].id;
  }

  // ------------------------------------------------------------- lifecycle
  var SCRIPTS = {
    tax: function (c) {
      return [].concat(
        CLI.codex.tool('Edited', 'src/checkout/total.js (+6 -2)'),
        CLI.codex.diff([[6, '-', '  return { subtotal, shipping, tax: 0, total: subtotal + shipping };'], [6, '+', '  const tax = salesTax(cart, address, rates);'], [7, '+', '  return { subtotal, shipping, tax, total: subtotal + shipping + tax };']]),
        CLI.codex.tool('Ran', 'npm test', '✔ 7 passed'),
        [blank()],
        CLI.codex.say('Tax is now its own line on the order total. Rates come from the shipping state, and exempt items are left out. I added tests for an exempt item and a state with no rate.')
      );
    },
    slash: function (c) {
      return [].concat(
        CLI.claude.tool('Update', 'src/router.rs', 'Added 9 lines, removed 2 lines'),
        CLI.claude.diff([[38, '+', '        // "/users/" and "/users" are the same route.'], [40, '+', '        let parts = if parts == [""] { vec![] } else { parts };']]),
        CLI.claude.tool('Bash', 'cargo test', 'test result: ok. 3 passed; 0 failed'),
        [blank()],
        CLI.claude.say('/users/ now matches the /users route, and the root path still matches "/". I added a test for both.')
      );
    },
    generic: function (c) {
      var p = c.project === 'storefront' ? 'src/checkout/total.js' : 'src/router.rs';
      var file = { path: p, status: 'Modified', add: 12, del: 4, rows: [[30, '-', '    // TODO'], [30, '+', '    // Handled for: ' + shorten(c.title || 'this task', 48)]] };
      c.files = c.files.concat([file]);
      return [].concat(
        CLI[c.cli].tool(c.cli === 'claude' ? 'Update' : 'Edited', p, c.cli === 'claude' ? 'Updated ' + p + ' with 12 additions and 4 removals' : '+12 −4'),
        CLI[c.cli].say('Done. This is the prototype: no agent ran, and the change is sample data.')
      );
    }
  };

  function finish(c) {
    var script = c.script || 'generic';
    c.lines = c.lines.concat(SCRIPTS[script](c));
    if (FILES[script]) c.files = FILES[script];
    c.status = 'ready';
    c.finishAt = null;
    c.script = null;
    c.seen = false;
    notify(c, 'Ready to check');
    if (c.worker) setTimeout(function () { leadDoorbell(c); }, 1500);
  }

  function submit(c, text) {
    var cl = CLI[c.cli];
    if (!c.named && c.status !== 'terminal') {
      c.title = shorten(text, 60);
      c.named = true;
      var taken = Object.keys(S.cards).map(function (k) { return S.cards[k].branch; });
      var b = slug(text), n = 2, base = b;
      while (taken.indexOf(b) >= 0) b = base + '-' + n++;
      c.branch = b;
    }
    c.lines = c.lines.concat(cl.prompt(text));
    c.status = 'working';
    c.startedAt = now();
    c.finishAt = now() + 7000 + Math.random() * 4000;
    c.script = c.script || 'generic';
    render();
  }

  function answer(c, i) {
    var opt = c.ask.options[i];
    c.lines = c.lines.concat([line(seg('  ⎿  ', 'c-dim'), seg(opt, 'c-white')), blank()]);
    c.ask = null;
    c.status = 'working';
    c.startedAt = now();
    c.finishAt = now() + 6000;
    c.script = 'slash';
    render();
  }

  function leadDoorbell(worker) {
    var L = S.lead;
    L.chat.push({ who: 'bell', text: '[shika] Workers changed: ' + worker.title + ' is Ready. Run shika wait.' });
    L.status = 'working';
    L.startedAt = now();
    L.step = 'Reading the diff of ' + shorten(worker.title, 40) + '...';
    render();
    setTimeout(function () {
      var stat = plural(worker.files.length, 'file') + ' +' + sum(worker.files, 'add') + ' −' + sum(worker.files, 'del');
      L.chat.push({ who: 'steps', text: ['shika wait', 'shika diff --stat ' + worker.branch] });
      L.chat.push({ who: 'lead', text: '`' + worker.branch + '` is ready: ' + stat + ', and `cargo test` passes. The text output is unchanged. Say the word and I will open the PR.' });
      L.status = 'ready';
      L.step = null;
      L.seen = false;
      notify(L, 'Ready to check');
      render();
    }, 4000);
  }

  function notify(c, body) {
    var title = c.project + ' - ' + (c.id === 'lead' ? 'Lead' : (c.title || 'New ' + cliOf(c.cli).name));
    var id = nextId('n');
    S.notes.unshift({ id: id, card: c.id, title: title, body: body });
    S.notes = S.notes.slice(0, 3);
    setTimeout(function () { S.notes = S.notes.filter(function (n) { return n.id !== id; }); render(); }, 6000);
  }

  var toastTimer;
  function toast(t) {
    clearTimeout(toastTimer);
    S.toast = t;
    toastTimer = setTimeout(function () { S.toast = null; render(); }, 3200);
    render();
  }

  // New: the picker, then a card that waits for its first prompt.
  function openPicker(pid) {
    if (S.overlay) return;
    S.overlay = { kind: 'picker', project: pid || currentProject(), idx: 0, worktree: true, focus: S.focus };
    render();
  }
  function rows() { return CLIS.concat([{ id: 'terminal', name: 'Terminal', path: 'Login shell' }]); }
  function create(o) {
    var row = rows()[o.idx], p = project(o.project);
    var draft = 'shika-draft-' + hex();
    var c = makeCard({
      id: nextId('c'), project: p.id, cli: row.id === 'terminal' ? 'pi' : row.id, branch: o.worktree ? draft : p.base,
      status: row.id === 'terminal' ? 'terminal' : 'waiting'
    });
    if (row.id === 'terminal') {
      c.title = 'Terminal ' + draft.slice(-4);
      c.named = true;
      c.isTerminal = true;
      c.lines = [];
    }
    c.checkout = !o.worktree;
    if (c.checkout) c.path = '~/code/' + p.id;
    S.cards[c.id] = c;
    p.cards.push(c.id);
    S.overlay = null;
    select(c.id, 'terminal');
  }

  // Close: ask before anything is lost, with exact Command chords.
  function requestClose(id) {
    var c = card(id);
    if (!c || S.overlay) return;
    if (id === 'lead') { toast('Stop the Lead from the native app. The prototype keeps it running.'); return; }
    var dirty = c.files.length > 0 && c.committed === 0;
    var unpushed = c.committed > c.pushed;
    if (c.status === 'working' || c.status === 'asking' || dirty || unpushed) {
      S.overlay = { kind: 'close', id: id, opened: now(), dirty: dirty, unpushed: unpushed, active: c.status === 'working' || c.status === 'asking', focus: S.focus };
      render();
    } else remove(id, 'Closed. Worktree removed.');
  }
  function remove(id, message) {
    var ids = order(), i = ids.indexOf(id), c = card(id);
    c.closing = true;
    S.overlay = null;
    render();
    setTimeout(function () {
      S.projects.forEach(function (p) { p.cards = p.cards.filter(function (x) { return x !== id; }); });
      S.lead.workers = S.lead.workers.filter(function (x) { return x !== id; });
      delete S.cards[id];
      S.notes = S.notes.filter(function (n) { return n.card !== id; });
      var rest = order();
      S.sel = rest.length ? rest[Math.min(i, rest.length - 1)] : null;
      S.selectedAt = now();
      S.focus = 'terminal';
      toast(message);
    }, 360);
  }

  // Create PR: preview, then commit, push, and open the PR against main.
  function openPublish() {
    var c = selected();
    if (!c || S.overlay || c.id === 'lead' || c.checkout || c.isTerminal) return;
    if (c.status === 'working' || c.status === 'asking') { toast('Wait for the agent to finish before creating a PR.'); return; }
    if (c.pr) { toast('Pull request #' + c.pr.n + ' is open.'); return; }
    S.overlay = { kind: 'publish', id: c.id, focus: S.focus };
    render();
  }
  function publish() {
    var c = card(S.overlay.id);
    S.overlay = null;
    c.committed = Math.max(1, c.committed);
    c.pushed = c.committed;
    c.pr = { n: 40 + Object.keys(S.cards).length, state: 'running' };
    toast('Opened pull request #' + c.pr.n + '.');
    setTimeout(function () { if (c.pr) { c.pr.state = 'passed'; render(); } }, 6000);
  }

  // Shell tabs: a few Git commands, simulated against the task's sample diff.
  function shell(c) { return c.tabs.filter(function (t) { return t.id === c.tab; })[0] || null; }
  function newShell() {
    var c = selected();
    if (!c || c.id === 'lead') return;
    var id = c.nextShell++;
    c.tabs.push({ id: id, label: id === 1 ? 'Shell' : 'Shell ' + id, lines: [], input: '' });
    c.tab = id;
    S.focus = 'terminal';
    render();
  }
  function closeShell(c, id) {
    var i = c.tabs.map(function (t) { return t.id; }).indexOf(id);
    if (!id || i < 0) return;
    c.tabs.splice(i, 1);
    if (c.tab === id) c.tab = i > 0 ? c.tabs[i - 1].id : 0;
    render();
  }
  function shellPrompt(c) { return [seg(c.project, 'c-blue'), seg(' ' + c.branch, 'c-dim'), seg(' $ ')]; }
  function runShell(c, sh) {
    var raw = sh.input, cmd = raw.trim(), out = [], dirty = c.files.length > 0 && c.committed === 0 && !c.cleaned;
    sh.lines.push(shellPrompt(c).concat([seg(raw, 'c-white')]));
    sh.input = '';
    if (!cmd) { /* nothing */ }
    else if (cmd === 'clear') sh.lines = [];
    else if (cmd === 'pwd') out.push(line(c.path.replace('~', '/Users/you')));
    else if (cmd === 'ls') out.push(line(c.project === 'storefront' ? 'README.md  package.json  src  test' : 'Cargo.toml  README.md  src  tests'));
    else if (/^git status/.test(cmd)) {
      out.push(line('On branch ' + c.branch));
      if (!dirty) out.push(line('nothing to commit, working tree clean'));
      else if (c.staged) { out.push(line('Changes to be committed:')); c.files.forEach(function (f) { out.push(line(seg('        modified:   ' + f.path, 'c-green'))); }); }
      else { out.push(line('Changes not staged for commit:')); c.files.forEach(function (f) { out.push(line(seg('        modified:   ' + f.path, 'c-red'))); }); }
    } else if (/^git diff/.test(cmd)) {
      if (dirty) c.files.forEach(function (f) {
        out.push(line(seg('diff --git a/' + f.path + ' b/' + f.path, 'b')));
        f.rows.forEach(function (r) { out.push(line(seg(r[1] + r[2], r[1] === '+' ? 'c-green' : r[1] === '-' ? 'c-red' : ''))); });
      });
    } else if (/^git add/.test(cmd)) { if (dirty) c.staged = true; }
    else if (/^git commit/.test(cmd)) {
      if (!dirty) out.push(line('nothing to commit, working tree clean'));
      else if (!c.staged && !/\s-\w*a/.test(cmd)) out.push(line('no changes added to commit (use "git add" and/or "git commit -a")'));
      else {
        var m = (cmd.match(/-\w*m\s+["'](.+?)["']/) || [])[1] || c.title || 'Update';
        out.push(line('[' + c.branch + ' ' + hex() + '] ' + m));
        out.push(line(' ' + plural(c.files.length, 'file') + ' changed, ' + sum(c.files, 'add') + ' insertions(+), ' + sum(c.files, 'del') + ' deletions(-)'));
        c.committed += 1; c.staged = false;
      }
    } else if (/^git push/.test(cmd)) {
      if (c.committed > c.pushed) {
        c.pushed = c.committed;
        out.push(line('To github.com:you/' + c.project + '.git'), line(' * [new branch]      ' + c.branch + ' -> ' + c.branch));
      } else out.push(line('Everything up-to-date'));
    } else if (/test/.test(cmd)) out.push(line(seg('✔ ', 'c-green'), 'all tests passed'));
    else out.push(line('zsh: command not found: ' + cmd.split(' ')[0]));
    sh.lines = sh.lines.concat(out);
    render();
  }

  // ----------------------------------------------------------------- keys
  function onKey(e) {
    var k = e.key, meta = e.metaKey, ctrl = e.ctrlKey, alt = e.altKey, shift = e.shiftKey;
    var o = S.overlay, c = selected();

    if (o && o.kind === 'close') {
      // Destructive and publishing choices are exact Command chords, after 300ms.
      if (k === 'Escape') { S.overlay = null; render(); }
      else if (now() - o.opened >= 300 && meta && !ctrl && !alt && !shift) {
        if (k.toLowerCase() === 'd') remove(o.id, 'Discarded. Worktree and branch deleted.');
        else if (k === 'Enter' && !o.dirty && o.unpushed && !o.active) { card(o.id).pushed = card(o.id).committed; remove(o.id, 'Pushed ' + card(o.id).branch + '. Card and worktree removed.'); }
      }
      e.preventDefault(); return;
    }
    if (o && o.kind === 'publish') {
      if (k === 'Escape') { S.overlay = null; render(); }
      else if (k === 'Enter' && !meta) publish();
      e.preventDefault(); return;
    }
    if (o && o.kind === 'picker') {
      var n = rows().length;
      if (k === 'Escape') { S.overlay = null; S.focus = o.focus; render(); }
      else if (k === 'j' || k === 'ArrowDown') { o.idx = Math.min(n - 1, o.idx + 1); render(); }
      else if (k === 'k' || k === 'ArrowUp') { o.idx = Math.max(0, o.idx - 1); render(); }
      else if (k === ' ') { o.worktree = !o.worktree; render(); }
      else if (k === 'Tab') {
        var i = S.projects.map(function (p) { return p.id; }).indexOf(o.project);
        o.project = S.projects[(i + (shift ? S.projects.length - 1 : 1)) % S.projects.length].id; render();
      } else if (k === 'Enter') create(o);
      else if (/^[1-5]$/.test(k) && +k <= n) { o.idx = +k - 1; create(o); }
      e.preventDefault(); return;
    }

    if (ctrl && !meta && k === 'Tab' && c && c.id !== 'lead') {
      var tabs = [0].concat(c.tabs.map(function (t) { return t.id; }));
      c.tab = tabs[(tabs.indexOf(c.tab) + (shift ? tabs.length - 1 : 1)) % tabs.length];
      S.focus = 'terminal'; render(); e.preventDefault(); return;
    }
    if (ctrl && !meta && k.toLowerCase() === 'q') { S.focus = 'cards'; render(); e.preventDefault(); return; }
    if (meta && !ctrl) {
      var lk = k.toLowerCase();
      if (alt && (lk === 'b' || e.code === 'KeyB')) { S.changesOpen = !S.changesOpen; S.focus = S.changesOpen ? 'changes' : 'terminal'; render(); }
      else if (lk === 'b') { S.colHidden = !S.colHidden; render(); }
      else if (lk === 'n') openPicker();
      else if (lk === 't') newShell();
      else if (lk === 'w' && shift) { if (c) requestClose(c.id); }
      else if (lk === 'w') { if (c && c.id !== 'lead') closeShell(c, c.tab); }
      else if (lk === 'p' && shift) openPublish();
      else if (lk === 'l') toast('routekit already has a Lead. Leads are one per project.');
      else if (k === ']' || k === '[') move(k === ']' ? 1 : -1, true);
      else if (/^[1-9]$/.test(k) && c && c.id !== 'lead') { var all = [0].concat(c.tabs.map(function (t) { return t.id; })); if (all[+k - 1] !== undefined) { c.tab = all[+k - 1]; S.focus = 'terminal'; render(); } }
      else return;
      e.preventDefault(); return;
    }
    if (meta || ctrl || alt) return;

    if (S.focus === 'changes') {
      if (k === 'Escape') { S.changesOpen = false; S.focus = 'terminal'; render(); }
      e.preventDefault(); return;
    }
    if (S.focus === 'terminal' && c) { termKey(e, c); return; }
    if (k === 'j' || k === 'ArrowDown') move(1);
    else if (k === 'k' || k === 'ArrowUp') move(-1);
    else if (k === 'Enter') { if (c) { S.focus = 'terminal'; render(); } }
    else return;
    e.preventDefault();
  }

  function termKey(e, c) {
    var k = e.key, printable = k.length === 1;
    if (k === 'Escape') { e.preventDefault(); return; }
    if (c.id === 'lead') {
      if (printable) c.input += k;
      else if (k === 'Backspace') c.input = c.input.slice(0, -1);
      else if (k === 'Enter' && c.input.trim()) { leadSend(c.input.trim()); c.input = ''; }
      else return;
      e.preventDefault(); render(); return;
    }
    var sh = shell(c);
    if (sh) {
      if (printable) sh.input += k;
      else if (k === 'Backspace') sh.input = sh.input.slice(0, -1);
      else if (k === 'Enter') { e.preventDefault(); runShell(c, sh); return; }
      else return;
      e.preventDefault(); render(); return;
    }
    if (c.ask) {
      var n = c.ask.options.length;
      if (k === 'ArrowDown') c.ask.choice = (c.ask.choice + 1) % n;
      else if (k === 'ArrowUp') c.ask.choice = (c.ask.choice + n - 1) % n;
      else if (k === 'Enter') { e.preventDefault(); answer(c, c.ask.choice); return; }
      else if (/^[1-9]$/.test(k) && +k <= n) { e.preventDefault(); answer(c, +k - 1); return; }
      else return;
      e.preventDefault(); render(); return;
    }
    if (c.status === 'working') { e.preventDefault(); return; }
    if (printable) c.input += k;
    else if (k === 'Backspace') c.input = c.input.slice(0, -1);
    else if (k === 'Enter') {
      e.preventDefault();
      var text = c.input.trim();
      c.input = '';
      if (c.isTerminal) { c.lines.push(shellPrompt(c).concat([seg(text, 'c-white')])); if (text) c.lines.push(line('zsh: this prototype runs no commands. Try a Shell tab with git status.')); render(); }
      else if (text) submit(c, text);
      return;
    } else return;
    e.preventDefault(); render();
  }

  function leadSend(text) {
    var L = S.lead;
    L.chat.push({ who: 'me', text: text });
    L.status = 'working';
    L.startedAt = now();
    L.step = 'Thinking...';
    setTimeout(function () {
      L.chat.push({ who: 'lead', text: 'In the app I would brief a worker for that and report back. This prototype has no model behind it, so this reply is sample text.' });
      L.status = 'ready';
      L.step = null;
      L.seen = false;
      render();
    }, 2600);
  }

  // ---------------------------------------------------------------- render
  function pixels() {
    // 25 pixels, each lit once per 2s cycle, in an order where neighbors
    // never light within four slots of each other. The negative delay keeps
    // the phase across re-renders.
    var phase = (now() % 2000) / 1000, html = '';
    for (var i = 0; i < 25; i++) {
      var slot = (i * 7 + Math.floor(i / 5) * 3) % 25;
      html += '<i style="animation-delay:' + ((slot / 25) * 2 - phase).toFixed(3) + 's"></i>';
    }
    return '<span class="pixels">' + html + '</span>';
  }
  function signal(c) {
    if (c.closing) return '';
    if (c.status === 'working') return pixels();
    if (c.status === 'asking') return '<span class="dot asking"></span>';
    if (c.status === 'waiting') return '<span class="dot waiting"></span>';
    if (c.status === 'ready' && !c.seen) return '<span class="dot ready"></span>';
    return '';
  }
  function statHtml(files) {
    if (!files.length) return '';
    var a = sum(files, 'add'), d = sum(files, 'del');
    return '<span class="sep">·</span><span class="stat">' + plural(files.length, 'file') + (a ? ' <span class="add">+' + a + '</span>' : '') + (d ? ' <span class="del">−' + d + '</span>' : '') + '</span>';
  }
  function hintFor(c) {
    if (S.sel !== c.id || S.focus !== 'cards') return '';
    var t = c.isTerminal ? 'terminal' : { ready: 'read', asking: 'answer', working: 'watch', waiting: 'write prompt' }[c.status];
    return '<span class="hint"><span class="kbd">↵</span>' + t + '</span>';
  }
  function cardHtml(c) {
    var sel = S.sel === c.id ? ' is-selected' : '';
    var title = c.title || 'New ' + cliOf(c.cli).name;
    var mark = c.isTerminal ? '<span>Terminal</span>' : '<span class="mark" title="' + esc(cliOf(c.cli).name) + '">' + MARK[c.cli] + '</span>';
    var l2 = mark;
    if (c.closing) l2 += '<span class="sep">·</span><span style="color:var(--ink-2);font-weight:500">Closing...</span>';
    if (c.named || c.isTerminal || c.checkout) l2 += '<span class="sep">·</span><span class="branch">' + esc(c.branch) + '</span>';
    if (c.status === 'ready' && !c.closing) l2 += statHtml(c.files);
    if (c.pr) l2 += '<span class="sep">·</span><span class="pr">#' + c.pr.n + (c.pr.state === 'running' ? '<span class="ring"></span>' : ' ✓') + '</span>';
    return '<div class="card' + sel + (c.closing ? ' is-closing' : '') + '" data-card="' + c.id + '"><div class="card-body">' +
      '<div class="line1"><span class="task' + (c.title ? '' : ' is-new') + '">' + esc(title) + '</span>' +
      (c.status === 'working' && !c.closing ? '<span class="timer">' + elapsed(now() - c.startedAt) + '</span>' : '') +
      '<span class="signal">' + signal(c) + '</span></div>' +
      '<div class="line2">' + l2 + hintFor(c) + '</div></div></div>';
  }
  function leadHtml() {
    var L = S.lead, sel = S.sel === 'lead' ? ' is-selected' : '';
    var last = L.chat.filter(function (m) { return m.who === 'lead'; }).pop();
    var l2 = L.status === 'working' && L.step ? L.step : (last ? last.text.replace(/`/g, '') : '');
    return '<div class="card' + sel + '" data-card="lead"><span class="lead-tile">' + ICON.antlers + '</span><div class="card-body">' +
      '<div class="line1"><span class="task">' + esc(L.project) + '</span>' +
      (L.status === 'working' ? '<span class="timer">' + elapsed(now() - L.startedAt) + '</span>' : '') +
      '<span class="signal">' + signal(L) + '</span></div>' +
      '<div class="line2"><span class="lead-reply">' + esc(l2) + '</span>' + hintFor(L) + '</div></div></div>';
  }

  function columnHtml() {
    var cur = currentProject();
    var groups = S.projects.slice().sort(function (a, b) { return (b.cards.length ? 1 : 0) - (a.cards.length ? 1 : 0); });
    var projects = groups.map(function (p) {
      return '<div class="project' + (p.id === cur ? ' is-current' : '') + '"><div class="project-head"><span class="name">' + esc(p.name) + '</span>' +
        '<span class="base">' + ICON.branch + esc(p.base) + '</span><span class="spacer"></span>' +
        '<button class="icon-btn" data-act="new" data-project="' + p.id + '" title="New agent ⌘N">' + ICON.plus + '</button></div>' +
        '<div class="cards">' + sorted(p.cards).map(cardHtml).join('') + '</div></div>';
    }).join('');
    return '<aside class="col"><div class="titlebar"><span class="lights"><i></i><i></i><i></i></span>' +
      '<span class="wordmark">Shika</span><button class="icon-btn" data-act="column" title="Hide agent column ⌘B">' + ICON.column + '</button></div>' +
      '<div class="list"><div class="section"><div class="section-head"><span class="label">Leads</span><button class="icon-btn" data-act="lead-new" title="New Lead ⌘L">' + ICON.plus + '</button></div>' +
      leadHtml() + '<div class="lead-workers">' + sorted(S.lead.workers).map(cardHtml).join('') + '</div></div>' +
      '<div class="section"><div class="section-head"><span class="label">Projects</span>' +
      '<button class="icon-btn" data-act="add-project" title="Add project ⌘O">' + ICON.folderPlus + '</button>' +
      '<button class="icon-btn" data-act="new" title="New agent ⌘N">' + ICON.plus + '</button></div>' +
      '<div class="projects-list">' + projects + '</div></div></div>' +
      '<div class="footer"><button class="icon-btn" data-act="settings" title="Settings ⌘,">' + ICON.gear + '</button>' +
      '<span class="hints"><span><span class="kbd">⌘] ⌘[</span>move</span><span><span class="kbd">⌘⇧W</span>close</span></span></div></aside>';
  }

  function lineHtml(l) {
    return '<div class="ln">' + l.map(function (s) { return s[1] ? '<span class="' + s[1] + '">' + esc(s[0]) + '</span>' : esc(s[0]); }).join('') + '</div>';
  }

  function agentLines(c) {
    var cl = CLI[c.cli], out = c.lines.slice();
    if (c.isTerminal) {
      return out.map(lineHtml).join('') + '<div class="ln">' + shellPrompt(c).map(function (s) { return '<span class="' + s[1] + '">' + esc(s[0]) + '</span>'; }).join('') + '<span class="c-white">' + esc(c.input) + '</span><span class="cursor"></span></div>';
    }
    if (c.ask) {
      out.push(line(seg('╭─ ', 'c-faint'), seg('Question', 'c-salmon b')));
      out.push(line(seg('│ ', 'c-faint'), c.ask.q));
      c.ask.options.forEach(function (o, i) {
        var on = i === c.ask.choice;
        out.push(line(seg('│ ', 'c-faint'), seg(on ? '❯ ' : '  ', 'c-salmon'), seg((i + 1) + '. ' + o, on ? 'c-white' : 'c-dim')));
      });
      out.push(line(seg('╰─ ', 'c-faint'), seg('↑↓ choose · enter select · esc cancel', 'c-dim')));
      return out.map(lineHtml).join('');
    }
    if (c.status === 'working') out = out.concat(cl.working(now() - c.startedAt));
    var html = out.map(lineHtml).join('');
    var input = '<span class="c-white">' + esc(c.input) + '</span><span class="cursor"></span>';
    if (cl.inputBox) {
      html += '<div class="ln" style="height:10px;min-height:0"></div><div style="border-top:1px solid var(--term-fainter);border-bottom:1px solid var(--term-fainter);padding:4px 0"><div class="ln"><span class="c-dim">❯ </span>' + input + (c.input || c.status !== 'waiting' ? '' : '<span class="c-faint">Try "how does stripe.js work?"</span>') + '</div></div>';
    } else {
      html += '<div class="ln"></div><div class="ln"><span class="c-dim">' + esc(cl.inputPrefix) + '</span>' + input + '</div>';
    }
    return html + lineHtml(cl.footer);
  }

  function chatHtml() {
    var L = S.lead;
    var stamp = new Date(L.startedAt).toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' });
    var msgs = '<div class="stamp">Today at ' + esc(stamp) + '</div>' + L.chat.map(function (m) {
      if (m.who === 'me') return '<div class="bubble me">' + esc(m.text) + '</div>';
      if (m.who === 'bell') return '<div class="steps"><span>' + esc(m.text) + '</span></div>';
      if (m.who === 'steps') return '<div class="steps">' + m.text.map(function (s) { return '<span>✓ ' + esc(s) + '</span>'; }).join('') + '</div>';
      return '<div class="bubble lead"><p>' + esc(m.text).replace(/`([^`]+)`/g, '<code>$1</code>') + '</p></div>';
    }).join('');
    if (L.status === 'working') msgs += '<div class="steps"><span>' + esc(L.step || 'Working...') + '</span></div>';
    var input = L.input ? '<span class="text">' + esc(L.input) + '<span class="cursor"></span></span>' : '<span class="text placeholder">Message Lead</span>';
    var hint = S.focus === 'terminal' ? '<div class="composer-hint">ctrl q back to cards</div>' : '';
    return '<div class="chat"><div class="chat-inner">' + msgs + '</div></div><div class="composer">' + input + '<span class="send">' + ICON.send + '</span></div>' + hint;
  }

  function mainHtml() {
    var c = selected(), head;
    var colReveal = '<span class="reveal-col"><span class="wordmark">Shika</span><button class="icon-btn" data-act="column" title="Show agent column ⌘B">' + ICON.column + '</button></span>';
    var changesBtn = S.changesOpen ? '' : '<button class="changes-toggle" data-act="changes" title="Show changes ⌥⌘B">' + ICON.changes + '</button>';
    if (!c) {
      return '<main class="main"><div class="term-head">' + colReveal + '<span style="flex:1"></span>' + changesBtn + '</div>' +
        '<div class="empty"><img src="assets/shika-app-icon-256.png" alt=""><span>No agent selected</span><span class="keys"><span><span class="k">j</span>select</span><span><span class="k">⌘N</span>new agent</span></span></div></main>';
    }
    var termFocus = S.focus === 'terminal' ? ' is-term-focus' : '';
    if (c.id === 'lead') {
      var ws = sorted(S.lead.workers), working = ws.filter(function (w) { return w.status === 'working'; }).length, ready = ws.filter(function (w) { return w.status === 'ready'; }).length;
      var count = working ? '<span class="dot-sm working"></span>' + working + ' working' : ready ? '<span class="dot-sm ready"></span>' + ready + ' ready' : 'No workers';
      head = '<div class="term-head lead-head">' + colReveal + '<span class="lead-title"><span class="lead-tile sm">' + ICON.antlers + '</span><b>' + esc(c.project) + '</b> Lead</span>' +
        '<span class="lead-count">' + count + '</span><button class="head-btn" data-act="close">Stop Lead <span class="k">⌘⇧W</span></button></div>';
      return '<main class="main' + termFocus + '" data-act="focus-term">' + head + chatHtml() + '</main>';
    }
    var tabs = [{ id: 0, label: c.isTerminal ? 'Terminal' : cliOf(c.cli).name }].concat(c.tabs).map(function (t) {
      return '<div class="tab' + (t.id === c.tab ? ' is-active' : '') + '" data-tab="' + t.id + '"><span>' + esc(t.label) + '</span>' +
        (t.id ? '<button class="tab-x" data-close-tab="' + t.id + '" title="Close shell tab (stops its processes) ⌘W">×</button>' : '') + '</div>';
    }).join('');
    head = '<div class="term-head">' + colReveal + '<div class="tabs">' + tabs + '<button class="add-tab" data-act="shell" title="New shell tab ⌘T">+</button></div>' +
      '<button class="head-btn" data-act="close">Close task <span class="k">⌘⇧W</span></button>' + changesBtn + '</div>';
    var pr = c.checkout || c.isTerminal ? '' : '<button class="head-btn" data-act="publish" title="Commit, push, create PR ⌘⇧P">Create PR <span class="k">⌘⇧P</span></button>';
    var meta = '<div class="meta"><span class="proj">' + esc(c.project) + '</span><span class="br" title="' + esc(c.path) + '">' + ICON.branch + '<span>' + esc(c.branch) + '</span></span>' +
      pr + '<span class="focus-hint">' + (termFocus ? 'ctrl q back to cards' : '↵ type here') + '</span></div>';
    var sh = shell(c), body;
    if (sh) body = sh.lines.map(lineHtml).join('') + '<div class="ln">' + shellPrompt(c).map(function (s) { return '<span class="' + s[1] + '">' + esc(s[0]) + '</span>'; }).join('') + '<span class="c-white">' + esc(sh.input) + '</span><span class="cursor"></span></div>';
    else body = agentLines(c);
    return '<main class="main' + termFocus + '">' + head + meta + '<div class="term" data-act="focus-term">' + body + '</div></main>';
  }

  function changesHtml() {
    if (!S.changesOpen) return '';
    var c = selected(), files = c && c.files ? c.files : [];
    var focus = S.focus === 'changes' ? ' is-focus' : '';
    var st = files.length ? plural(files.length, 'file') + ' <span style="color:var(--added)">+' + sum(files, 'add') + '</span> <span style="color:var(--removed)">−' + sum(files, 'del') + '</span>' : '';
    var list;
    if (!c) list = '<div class="changes-empty">No agent selected</div>';
    else if (c.id === 'lead') list = '<div class="changes-empty">The Lead does not change files</div>';
    else if (!files.length || c.committed && c.pushed) list = '<div class="changes-empty">No changes</div>';
    else list = '<div class="changes-list">' + files.map(function (f) {
      var dir = f.path.split('/').slice(0, -1).join('/'), name = f.path.split('/').pop();
      var rowsHtml = f.rows.map(function (r, i) {
        var gap = i > 0 && Math.abs(r[0] - f.rows[i - 1][0]) > 2 ? '<div class="dl gap"></div>' : '';
        var kind = r[1] === '+' ? ' add' : r[1] === '-' ? ' del' : '';
        return gap + '<div class="dl' + kind + '"><span class="no">' + r[0] + '</span>' + esc(r[2]) + '</div>';
      }).join('');
      return '<div class="file"><div class="file-head"><div class="r1"><span>⌄</span><span>' + esc(name) + '</span><span class="sp"></span>' +
        (f.add ? '<span class="n" style="color:var(--added)">+' + f.add + '</span>' : '') + (f.del ? '<span class="n" style="color:var(--removed)">−' + f.del + '</span>' : '') + '</div>' +
        '<div class="r2"><span>' + esc(dir) + '</span><span class="s">' + f.status + '</span></div></div>' + rowsHtml + '<div class="dl" style="height:21px"></div></div>';
    }).join('') + '</div>';
    return '<section class="changes' + focus + '"><div class="changes-head"><span class="title">Changes</span><span class="st">' + st + '</span>' +
      (focus ? '<span class="hints"><span><span class="k">esc</span>close</span></span>' : '') +
      '<button class="changes-toggle" data-act="changes" title="Hide changes ⌥⌘B">' + ICON.changes + '</button></div>' + list + '</section>';
  }

  function overlayHtml() {
    var o = S.overlay;
    if (!o) return '';
    if (o.kind === 'picker') {
      var p = project(o.project);
      var list = rows().map(function (r, i) {
        return (r.id === 'terminal' ? '<div class="row-sep"></div>' : '') +
          '<div class="row' + (i === o.idx ? ' is-active' : '') + '" data-pick="' + i + '"><span class="kbd">' + (i + 1) + '</span><span class="nm">' + r.name + '</span><span class="pth">' + r.path + '</span></div>';
      }).join('');
      var fact = o.worktree ? '' : '<div class="checkout-fact">Runs in your project folder on <code>' + esc(p.base) + '</code></div>';
      return '<div class="scrim" data-act="dismiss"><div class="panel picker" data-stop><div class="picker-head"><span class="t">New agent</span><span class="p">in ' + esc(p.name) + '</span><span class="tab-hint">tab to change project</span></div>' +
        list + fact + '<div class="picker-foot"><span>↵ start</span><span>esc cancel</span><button class="switch' + (o.worktree ? '' : ' is-off') + '" data-act="worktree" title="New branch from origin/' + esc(p.base) + '. Space to toggle.">Create worktree<span class="track"></span></button></div></div></div>';
    }
    if (o.kind === 'close') {
      var c = card(o.id), facts = [];
      if (o.active) facts.push('The agent is still working.');
      if (o.dirty) facts.push('The worktree has ' + plural(c.files.length, 'uncommitted file') + ': <code>' + c.files.map(function (f) { return esc(f.path); }).join('</code>, <code>') + '</code>.');
      if (o.unpushed) facts.push('The branch has commits that are not on the remote.');
      facts.push('Discard stops all task terminals and deletes its worktree and local branch.');
      var canPush = !o.dirty && o.unpushed && !o.active;
      return '<div class="scrim"><div class="panel dialog"><span class="t">Close “' + esc(shorten(c.title || 'New ' + cliOf(c.cli).name, 44)) + '”?</span>' +
        facts.map(function (f) { return '<span class="f">' + f + '</span>'; }).join('') +
        '<div class="buttons"><button class="btn" data-act="cancel">Cancel<span class="k">esc</span></button>' +
        '<button class="btn' + (canPush ? '' : ' primary') + '" data-act="discard">Discard<span class="k">⌘D</span></button>' +
        (canPush ? '<button class="btn primary" data-act="push">Push<span class="k">⌘↵</span></button>' : '') + '</div></div></div>';
    }
    if (o.kind === 'publish') {
      var pc = card(o.id);
      return '<div class="scrim"><div class="panel dialog"><span class="t">Create PR?</span>' +
        '<span class="f">Publishes <code>' + esc(pc.branch) + '</code> to <code>you/' + esc(pc.project) + '</code>.</span>' +
        '<span class="label">Commit and PR title</span><div class="field">' + esc(pc.title || pc.branch) + '</div>' +
        '<span class="label">PR target</span><div class="field" style="box-shadow:0 0 0 1px var(--line-control)">main</div>' +
        '<span class="label">Files</span><div class="files">' + (pc.files.length ? pc.files.map(function (f) { return '<div><span>' + esc(f.path) + '</span><span>+' + f.add + ' −' + f.del + '</span></div>'; }).join('') : '<div>Nothing new to commit</div>') + '</div>' +
        '<div class="buttons"><button class="btn" data-act="cancel">Cancel<span class="k">esc</span></button><button class="btn primary" data-act="publish-go">Commit, push, create PR<span class="k">↵</span></button></div></div></div>';
    }
    return '';
  }

  function notesHtml() {
    return '<div class="notes">' + S.notes.map(function (n) {
      return '<div class="note" data-note="' + n.id + '"><img src="assets/shika-app-icon-256.png" alt=""><div class="body"><div class="meta-row"><b>SHIKA</b><span>now</span></div>' +
        '<div class="ttl">' + esc(n.title) + '</div><div class="msg">' + esc(n.body) + '</div></div></div>';
    }).join('') + '</div>';
  }

  var app = document.getElementById('app');
  var lastScroll = '';
  function render() {
    var html = '<div class="backdrop"></div>' + columnHtml() + mainHtml() + changesHtml() + overlayHtml() +
      (S.toast ? '<div class="toast">' + esc(S.toast) + '</div>' : '') + notesHtml();
    app.className = 'win' + (S.colHidden ? ' col-hidden' : '');
    app.innerHTML = html;
    var term = app.querySelector('.term, .chat');
    var c = selected();
    if (term && c) {
      var sh = c.tabs ? shell(c) : null;
      var key = c.id + ':' + (c.tab || 0) + ':' + (sh ? sh.lines.length + sh.input : (c.lines ? c.lines.length : 0) + (c.chat ? c.chat.length : 0) + c.input + c.status);
      if (key !== lastScroll) lastScroll = key;
      term.scrollTop = term.scrollHeight;
    }
  }


  // ---------------------------------------------------------------- clicks
  app.addEventListener('click', function (e) {
    var t = e.target.closest('[data-act],[data-card],[data-tab],[data-close-tab],[data-pick],[data-note],[data-stop]');
    if (!t) return;
    if (t.hasAttribute('data-stop') && !e.target.closest('[data-pick],[data-act]')) return;
    var c = selected();
    if (t.dataset.closeTab) { e.stopPropagation(); closeShell(c, +t.dataset.closeTab); return; }
    if (t.dataset.tab !== undefined) { c.tab = +t.dataset.tab; S.focus = 'terminal'; render(); return; }
    if (t.dataset.card) { if (S.overlay) return; select(t.dataset.card, 'terminal'); return; }
    if (t.dataset.pick !== undefined) { S.overlay.idx = +t.dataset.pick; create(S.overlay); return; }
    if (t.dataset.note) {
      var n = S.notes.filter(function (x) { return x.id === t.dataset.note; })[0];
      S.notes = S.notes.filter(function (x) { return x !== n; });
      if (n && card(n.card)) select(n.card, 'terminal'); else render();
      return;
    }
    switch (t.dataset.act) {
      case 'column': S.colHidden = !S.colHidden; render(); break;
      case 'changes': S.changesOpen = !S.changesOpen; S.focus = S.changesOpen ? 'changes' : 'terminal'; render(); break;
      case 'new': e.stopPropagation(); openPicker(t.dataset.project); break;
      case 'lead-new': toast('routekit already has a Lead. Leads are one per project.'); break;
      case 'add-project': toast('Add project opens the folder picker here.'); break;
      case 'settings': toast('Settings opens here: theme, glass, background image, agents, and usage.'); break;
      case 'shell': newShell(); break;
      case 'close': if (c) requestClose(c.id); break;
      case 'publish': openPublish(); break;
      case 'publish-go': publish(); break;
      case 'worktree': S.overlay.worktree = !S.overlay.worktree; render(); break;
      case 'cancel': S.overlay = null; render(); break;
      case 'discard': if (now() - S.overlay.opened >= 300) remove(S.overlay.id, 'Discarded. Worktree and branch deleted.'); break;
      case 'push': var pc = card(S.overlay.id); pc.pushed = pc.committed; remove(pc.id, 'Pushed ' + pc.branch + '. Card and worktree removed.'); break;
      case 'dismiss': if (e.target === t) { S.focus = S.overlay.focus; S.overlay = null; render(); } break;
      case 'focus-term': if (c && !S.overlay) { S.focus = 'terminal'; render(); } break;
    }
  });

  // ---------------------------------------------------------------- clock
  setInterval(function () {
    var t = now(), changed = false;
    Object.keys(S.cards).forEach(function (k) {
      var c = S.cards[k];
      if (c.status === 'working') { changed = true; if (c.finishAt && t >= c.finishAt) finish(c); }
    });
    if (S.lead.status === 'working') changed = true;
    // A Ready result is seen when its terminal has focus, or after it stays
    // selected for one second; passing over it with j does not count.
    var s = selected();
    if (s && s.status === 'ready' && !s.seen && (S.focus === 'terminal' || t - S.selectedAt >= 1000)) { s.seen = true; changed = true; }
    if (changed) render();
  }, 250);

  // ---------------------------------------------------------------- theme
  function applyTheme(t) {
    if (t !== 'light' && t !== 'dark') t = window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
    root.dataset.theme = t;
  }
  applyTheme(params.get('theme'));
  root.dataset.glass = params.get('glass') === 'off' ? 'off' : 'on';
  if (embedded) root.classList.add('is-embedded');
  window.addEventListener('message', function (e) {
    if (e.origin !== location.origin || !e.data || e.data.type !== 'shika-theme') return;
    applyTheme(e.data.theme);
  });

  // Standalone: center the window and scale it to the viewport.
  function fit() {
    if (embedded) return;
    var s = Math.min(1, (window.innerWidth - 48) / 1400, (window.innerHeight - 48) / 880);
    app.style.transform = 'scale(' + s + ')';
  }
  window.addEventListener('resize', fit);
  fit();

  window.addEventListener('keydown', onKey);
  render();
})();

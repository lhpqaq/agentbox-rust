import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
import {performance} from 'node:perf_hooks';
const source=await fs.readFile(new URL('./ansi-text.js',import.meta.url),'utf8');
const {stripAnsi}=await import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));
for(const [input,expected] of [
 ['plain 中文 🙂\nnext\tcolumn','plain 中文 🙂\nnext\tcolumn'],
 ['\x1b[31mred\x1b[0m plain','red plain'],
 ['before\x1b]0;window title\x07after','beforeafter'],
 ['before\x1b]0;window title\x1b\\after','beforeafter'],
 ['\x1b]8;;https://example.com\x1b\\link label\x1b]8;;\x1b\\','link label'],
 ['before\x1b]unterminated\nsecret','before'],
 ['before\x1bPprivate payload\x1b\\after','beforeafter'],
 ['before\x1bXprivate payload\x1b\\after','beforeafter'],
 ['before\x1b^private payload\x1b\\after','beforeafter'],
 ['before\x1b_private payload\x1b\\after','beforeafter'],
 ['\x1b(Bnormal\x1b7saved\x1b8restored','normalsavedrestored'],
 ['a\r\nb\rc\nd','a\nb\nc\nd'],
 ['a\x00\x07\x08\x7fb','ab'],
 ['a\x1b[123;','a'],['a\x1b','a'],
 ['\x9b31mred\x9b0m','red'],['a\x9dtitle\x9cb','ab'],
 ['before\x1b]cancelled\x18after','beforeafter'],
 ['\x1b[中文 remains','中文 remains'],['\x1b🙂 remains','🙂 remains']
])assert.equal(stripAnsi(input),expected,JSON.stringify(input));
const malformed='prefix'+'\x1b]'.repeat(500000);
const start=performance.now();assert.equal(stripAnsi(malformed),'prefix');const elapsed=performance.now()-start;
assert.ok(elapsed<3000,`One-million-character malformed OSC must complete in <3s, took ${elapsed.toFixed(1)}ms`);
const plain='中文🙂 text\n'.repeat(100000);assert.equal(stripAnsi(plain),plain);
const mixed=('text\x1b[31mred\x1b[0m\x1b]title\x07\n').repeat(20000);
assert.equal(stripAnsi(mixed),'textred\n'.repeat(20000));
const app=await fs.readFile(new URL('./app.js',import.meta.url),'utf8');
assert.ok(app.includes("import { stripAnsi } from './ansi-text.js'"));
const handler=app.slice(app.indexOf("$('session-form').onsubmit="),app.indexOf("$('rename').onclick"));
assert.ok(handler.includes('requestGeneration=generation'));
assert.ok(handler.includes("requestGeneration===generation&&!!token&&!$('app').inert"));
assert.ok(handler.indexOf('if(!stillCurrent())return;')<handler.indexOf('sessions.unshift(s)'));
assert.ok(handler.includes("catch(e){if(stillCurrent())"));
console.log(`PASS: forward-only ANSI/OSC/DCS scanner, BEL/ST, Unicode/newlines, unterminated strings; 1M malformed characters in ${elapsed.toFixed(1)}ms. Session creation generation/unlock guard statically verified; no browser/socket test.`);

import assert from 'node:assert/strict';
import fs from 'node:fs/promises';
const source=await fs.readFile(new URL('./link-safety.js',import.meta.url),'utf8');
const {validateTerminalLink,createTerminalLinkHandler}=await import('data:text/javascript;base64,'+Buffer.from(source).toString('base64'));
const denied=[
 'javascript:alert(1)','JaVaScRiPt:alert(1)',' javascript:alert(1)','java\tscript:alert(1)',
 'java\nscript:alert(1)','\u0000javascript:alert(1)','data:text/html,<script>alert(1)</script>',
 'blob:https://example.com/id','file:///etc/passwd','ftp://example.com/file','about:blank',
 '//example.com/path','/local','example.com','https:example.com','https:/example.com',
 'https://user:password@example.com','https://user@example.com','https://@example.com',
 'https://user%40name@example.com','https://example.com\\@evil.example','https://example.com\n.evil.example',
 'https://example.com\t',' https://example.com','https://','http://?foo',null,{},'',
 'https://example.com/ raw space','https://example.com/\u007f'
];
for(const url of denied)assert.equal(validateTerminalLink(url),null,`Must block ${String(url)}`);
for(const [url,normalized] of [
 ['https://example.com/path?q=hello%20world#anchor','https://example.com/path?q=hello%20world#anchor'],
 ['HTTPS://EXAMPLE.COM','https://example.com/'],['http://localhost:8765/path','http://localhost:8765/path'],
 ['https://example.com/path/@name','https://example.com/path/@name'],['http://[::1]:80/','http://[::1]/']
])assert.equal(validateTerminalLink(url),normalized);
const opened=[],prompts=[],blocked=[];
let consent=false,prevented=0;
const handler=createTerminalLinkHandler({
 confirmOpen:message=>{prompts.push(message);return consent;},
 openLink:(...args)=>opened.push(args),showBlocked:message=>blocked.push(message)
});
assert.equal(handler.allowNonHttpProtocols,true,'All OSC 8 targets must reach the explicit validator and blocked notice');
const event={preventDefault(){prevented++;}};
for(const url of denied)handler.activate(event,url);
assert.equal(opened.length,0);assert.equal(prompts.length,0);assert.equal(blocked.length,denied.length);
handler.activate(event,'https://example.com');
assert.equal(opened.length,0,'Cancel must never open a URL');
assert.ok(prompts[0].includes('是否')&&prompts[0].includes('https://example.com/'));
consent=true;handler.activate(event,'https://example.com');
assert.deepEqual(opened,[['https://example.com/','_blank','noopener,noreferrer']]);
assert.equal(prevented,denied.length+2);
// Constructor integration must explicitly replace xterm's default OSC 8 handler.
const app=await fs.readFile(new URL('./app.js',import.meta.url),'utf8');
assert.ok(app.includes('new Xterm.Terminal({linkHandler:createTerminalLinkHandler('));
console.log('PASS: non-web/obfuscated/relative/credentialed terminal links blocked; explicit HTTP(S) only; Chinese confirmation required; cancellation respected; opener/referrer isolation configured. No URLs were opened.');

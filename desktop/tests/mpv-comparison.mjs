// Decoder-only control measurement; does not claim to validate libmpv embedding or GPU presentation.
import assert from 'node:assert/strict';
import {createHash,randomUUID} from 'node:crypto';
import {createReadStream} from 'node:fs';
import {mkdir,mkdtemp,realpath,writeFile} from 'node:fs/promises';
import {dirname,join,resolve,sep} from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawn} from 'node:child_process';
import net from 'node:net';
import {setTimeout as sleep} from 'node:timers/promises';
const args={};for(let i=2;i<process.argv.length;i+=2){assert.ok(['--mpv','--media-file','--expected-sha256'].includes(process.argv[i]));args[process.argv[i].slice(2)]=process.argv[i+1];}
assert.ok(args.mpv&&args['media-file']&&args['expected-sha256']);
const desktop=resolve(dirname(fileURLToPath(import.meta.url)),'..'),exe=await realpath(resolve(args.mpv)),file=await realpath(resolve(args['media-file']));assert.ok(exe.startsWith(join(desktop,'build','mpv-benchmark')+sep));
const hash=async path=>{const h=createHash('sha256');for await(const bytes of createReadStream(path))h.update(bytes);return h.digest('hex');};assert.equal(await hash(exe),args['expected-sha256'].toLowerCase());const before=await hash(file);
const root=resolve(process.env.STREAMCAP_TEST_ARTIFACTS||join(desktop,'tests','artifacts'));assert.ok(root===join(desktop,'tests','artifacts')||root.startsWith(join(desktop,'tests','artifacts')+sep));await mkdir(root,{recursive:true});const output=await mkdtemp(join(root,'mpv-comparison-'));
const pipe='\\\\.\\pipe\\streamcap-'+randomUUID();
const child=spawn(exe,['--no-config','--load-scripts=no','--ytdl=no','--idle=yes','--pause=yes','--mute=yes','--vo=null','--ao=null','--hwdec=no','--keep-open=yes','--input-ipc-server='+pipe],{cwd:output,windowsHide:true,stdio:'ignore'});
let socket,buffer='',id=0;const pending=new Map();let frame=0;const report={passed:false,method:'mpv CPU decoder with null video output; not full embedded player',file,sha256:before,seeks:[],output};
const command=(command)=>new Promise((yes,no)=>{const request_id=++id;const timer=setTimeout(()=>{pending.delete(request_id);no(Error('mpv command timeout'));},10000);pending.set(request_id,{yes:v=>{clearTimeout(timer);yes(v);},no});socket.write(JSON.stringify({command,request_id})+'\n');});
const wait=async(read,predicate,timeout=10000)=>{const until=Date.now()+timeout;while(Date.now()<until){const value=await read();if(predicate(value))return value;await sleep(20);}throw Error('mpv frame timeout');};
try{
 for(let n=0;n<100&&!socket;n++){socket=await new Promise(yes=>{const s=net.connect(pipe);s.once('connect',()=>yes(s));s.once('error',()=>{s.destroy();yes(null);});});if(!socket)await sleep(100);}
 assert.ok(socket,'mpv IPC ready');socket.on('error',()=>{});socket.on('data',chunk=>{buffer+=chunk;let end;while((end=buffer.indexOf('\n'))>=0){const line=buffer.slice(0,end);buffer=buffer.slice(end+1);if(!line.trim())continue;const value=JSON.parse(line);if(value.event==='playback-restart')frame++;const p=pending.get(value.request_id);if(p){pending.delete(value.request_id);if(value.error==='success')p.yes(value.data);else p.no(Error(value.error));}}});
 report.version=await command(['get_property','mpv-version']);const began=performance.now();await command(['loadfile',file,'replace']);await wait(()=>frame,n=>n>0);report.firstFrameMs=Math.round(performance.now()-began);report.duration=await command(['get_property','duration']);report.video=await command(['get_property','video-params']);
 for(const fraction of [.75,.2,.9]){const target=report.duration*fraction,previous=frame,start=performance.now();await command(['seek',target,'absolute+exact']);await wait(()=>frame,n=>n>previous);const actual=await command(['get_property','time-pos']);assert.ok(Math.abs(actual-target)<1);report.seeks.push({target,actual,decodedFrameMs:Math.round(performance.now()-start)});}
 report.passed=true;
}catch(e){report.failure=String(e);process.exitCode=1;}
finally{if(socket){socket.write(JSON.stringify({command:['quit']})+'\n');socket.end();}for(let n=0;n<100&&child.exitCode===null;n++)await sleep(50);if(child.exitCode===null){child.kill();report.passed=false;report.failure='mpv required owned-process termination';process.exitCode=1;}report.originalHashUnchanged=(await hash(file))===before;if(!report.originalHashUnchanged){report.passed=false;process.exitCode=1;}await writeFile(join(output,'result.json'),JSON.stringify(report,null,2));console.log(JSON.stringify(report,null,2));}

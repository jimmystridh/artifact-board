'use strict';
// Run with: node --test assets/bridge.test.cjs
const {test}=require('node:test');
const assert=require('node:assert/strict');
const fs=require('node:fs');
const vm=require('node:vm');
const source=fs.readFileSync(require('node:path').join(__dirname,'bridge.js'),'utf8');
const NONCE='a'.repeat(48);
function harness(pathname='/content/capability/revision/docs/index.html'){
 const windowEvents={},documentEvents={},messages=[];
 class Element{
  constructor(tag='P'){this.tagName=tag;this.style={outline:''};this.dataset={};this.id='intro';this.textContent='A thoughtful daily planner';this.parentElement=null;this.children=[];this.removed=false}
  closest(){return null} getAttribute(){return null} setAttribute(){} addEventListener(){} append(){} remove(){this.removed=true} scrollIntoView(){this.scrolled=true} getBoundingClientRect(){return {top:50,right:200}}
 }
 const body=new Element('BODY');body.id='';
 let selected='A thoughtful daily planner',collapsed=false,lookups=0;
 const document={body,documentElement:{clientWidth:800},addEventListener:(name,callback)=>{documentEvents[name]=callback},createRange:()=>({selectNodeContents(){},setEnd(){this.before=true},setStart(){},toString(){return this.before?'prefix '.repeat(30):'suffix '.repeat(30)}}),getElementById:()=>{lookups++;return null},querySelectorAll:()=>[],createTreeWalker:()=>({nextNode:()=>null}),createElement:tag=>new Element(tag.toUpperCase())};
 const parent={postMessage:(message,target)=>messages.push({message,target})};
 const context={window:{addEventListener:(name,callback)=>{windowEvents[name]=callback}},document,parent,HTMLElement:Element,CSS:{escape:x=>x},NodeFilter:{SHOW_TEXT:4,FILTER_ACCEPT:1,FILTER_REJECT:2},getSelection:()=>({isCollapsed:collapsed,rangeCount:1,toString:()=>selected,getRangeAt:()=>({startContainer:body,endContainer:body,startOffset:0,endOffset:1})}),location:{pathname},scrollX:0,scrollY:0,setTimeout:callback=>callback(),console};
 vm.runInNewContext(source,context,{timeout:1000});
 function message(data,source=parent){windowEvents.message({source,data:{source:'artifact-shell',nonce:NONCE,...data}})}
 function event(name,target=new Element()){const event={target,key:'',prevented:false,stopped:false,preventDefault(){this.prevented=true},stopImmediatePropagation(){this.stopped=true}};documentEvents[name](event);return event}
 return {message,event,messages,parent,Element,documentEvents,setSelection:(s,c=false)=>{selected=s;collapsed=c},get lookups(){return lookups}};
}
test('handshake requires exact parent and bounded random nonce; nonce stays pinned',()=>{
 const h=harness();h.message({type:'hello'},{});assert.equal(h.messages.length,0);
 h.message({type:'hello',nonce:'short'});assert.equal(h.messages.length,0);
 h.message({type:'hello'});assert.equal(h.messages.length,1);assert.equal(h.messages[0].message.type,'ready');assert.equal(h.messages[0].message.nonce,NONCE);
 h.message({type:'hello',nonce:'b'.repeat(48)});assert.equal(h.messages.length,1);
 h.message({type:'hello'});assert.equal(h.messages.length,2);
});
test('ordinary clicks remain untouched; explicit element mode intercepts only after authenticated handshake',()=>{
 const h=harness();assert.equal(h.event('click').prevented,false);
 h.message({type:'mode',mode:'element'});assert.equal(h.event('click').prevented,false);
 h.message({type:'hello'});h.message({type:'mode',mode:'element',nonce:'b'.repeat(48)});assert.equal(h.event('click').prevented,false);
 h.message({type:'mode',mode:'element'});const click=h.event('click');assert.equal(click.prevented,true);assert.equal(click.stopped,true);
 const selection=h.messages.find(x=>x.message.type==='selection').message;assert.equal(selection.anchor.kind,'element');assert.equal(selection.anchor.path,'docs/index.html');assert.equal(selection.anchor.element_id,'intro');assert.equal(h.event('click').prevented,false);
});
test('text capture is bounded and context is bounded, with revision-relative paths',()=>{
 const h=harness();h.message({type:'hello'});h.setSelection('x'.repeat(2001));h.event('mouseup');assert.equal(h.messages.length,1);
 h.setSelection('');h.event('mouseup');assert.equal(h.messages.length,1);
 h.setSelection('A thoughtful daily planner',true);h.event('mouseup');assert.equal(h.messages.length,1);
 h.setSelection('A thoughtful daily planner');h.event('mouseup');const anchor=h.messages[1].message.anchor;assert.equal(anchor.kind,'text');assert.equal(anchor.exact,'A thoughtful daily planner');assert.ok(anchor.prefix.length<=100);assert.ok(anchor.suffix.length<=100);assert.equal(anchor.path,'docs/index.html');
});
test('Escape cancels element mode without modifying normal click behavior',()=>{
 const h=harness();h.message({type:'hello'});h.message({type:'mode',mode:'element'});h.documentEvents.keydown({key:'Escape'});assert.equal(h.messages.at(-1).message.type,'cancel');assert.equal(h.event('click').prevented,false);
});
test('oversized placement batches and invalid selectors are ignored',()=>{
 const h=harness();h.message({type:'hello'});h.message({type:'placements',threads:Array.from({length:101},(_,i)=>({id:String(i),anchor:{kind:'element',element_id:'intro'}}))});assert.equal(h.lookups,0);
 h.message({type:'focus',anchor:{kind:'element',selector:'x'.repeat(501),element_id:'intro'}});assert.equal(h.lookups,0);
 h.message({type:'focus',anchor:{kind:'element',element_id:'intro'}},{});assert.equal(h.lookups,0);
});

test('gated frame ticket paths preserve only the artifact-relative path',()=>{
 const h=harness('/frame/capability/revision/signed-ticket/docs/hello%20world.html');h.message({type:'hello'});h.event('mouseup');assert.equal(h.messages[1].message.anchor.path,'docs/hello world.html');assert.ok(!JSON.stringify(h.messages[1]).includes('signed-ticket'));
});

test('anchors stay in their original artifact document despite shared IDs',()=>{
 const h=harness('/frame/capability/revision/ticket/docs/one.html');h.message({type:'hello'});
 h.message({type:'focus',anchor:{kind:'element',element_id:'intro',path:'docs/two.html'}});assert.equal(h.lookups,0);
 h.message({type:'placements',threads:[{id:'thread',anchor:{kind:'element',element_id:'intro',path:'docs/two.html'}}]});assert.equal(h.lookups,0);
 h.message({type:'focus',anchor:{kind:'element',element_id:'intro',path:{}}});assert.equal(h.lookups,0);
 h.message({type:'focus',anchor:{kind:'element',element_id:'intro',path:'docs/one.html'}});assert.equal(h.lookups,1);
});

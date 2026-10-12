import {isTerminalJob,isFailedJob} from './status.mjs';
import {score} from './score.mjs';
import {prepareFile} from './document.mjs';
const $=id=>document.getElementById(id);
let selected=null,reference=null,previewUrl=null,lastResult=null,busy=false,runNumber=0,uploadLimits;
const pause=ms=>new Promise(resolve=>setTimeout(resolve,ms));

function setBusy(value){busy=value;$('inputs').disabled=value;$('run').disabled=value||!selected;}
function error(message){$('error').textContent=message;$('error').hidden=false;}
function stage(name,message){$('progress').hidden=false;$('status').textContent=message;let reached=false;for(const item of document.querySelectorAll('[data-stage]')){item.classList.toggle('active',!reached);if(item.dataset.stage===name)reached=true;}}

async function request(path,{token,method='GET',body,headers={}}={}){
  const options={method,headers:{...headers},signal:AbortSignal.timeout(100000),cache:'no-store'};
  if(token)options.headers.Authorization=`Bearer ${token}`;
  if(body instanceof Blob)options.body=body;
  else if(body!==undefined){options.body=JSON.stringify(body);options.headers['Content-Type']='application/json';}
  const response=await fetch(path,options);
  const value=response.status===204?null:await response.json().catch(()=>null);
  if(!response.ok){const failure=new Error(value?.error?.message??`The request could not complete (HTTP ${response.status}).`);failure.status=response.status;throw failure;}
  return {value,response};
}

async function poll(path,token,states){
  const end=Date.now()+240000;
  while(Date.now()<end){
    try{const {value}=await request(path,{token});if(states(value.status))return value;}
    catch(failure){if(failure.status!==429)throw failure;await pause(60000);continue;}
    await pause(2500);
  }
  throw new Error('This document is taking longer than expected. The sandbox will finish or cancel the job and clean up its images automatically.');
}

async function choose(file,sample=null){
  $('error').hidden=true;
  selected=null;reference=null;setBusy(true);
  if(previewUrl)URL.revokeObjectURL(previewUrl);
  previewUrl=null;$('preview-wrap').hidden=true;
  try{
    uploadLimits ??= (await request('/limits.json')).value;
    file=await prepareFile(file,uploadLimits?.max_bytes);
  }catch(failure){error(failure.message);setBusy(false);return;}
  selected=file;reference=sample;
  const pdf=file.type==='application/pdf',noPreview=pdf||file.type==='image/tiff';
  $('pdf-preview').textContent=pdf?`PDF selected. Up to ${uploadLimits.max_pages} pages.`:'TIFF selected. Browser preview is unavailable.';
  $('preview').hidden=noPreview;$('pdf-preview').hidden=!noPreview;
  if(!noPreview){previewUrl=URL.createObjectURL(file);$('preview').src=previewUrl;}
  $('preview-wrap').hidden=false;
  $('file-title').textContent='Change selected document';$('file-hint').textContent='Choose another file or drop it here';
  $('file-meta').textContent=`${file.name} · ${(file.size/1024).toFixed(0)} KiB${sample?' · benchmark sample':''}`;
  $('nutrition').checked=Boolean(sample?.extraction);setBusy(false);
}

$('dropzone').addEventListener('click',()=>$('file').click());
$('file').addEventListener('change',async event=>{if(event.target.files[0])await choose(event.target.files[0]);});
for(const name of ['dragover','dragleave','drop'])$('dropzone').addEventListener(name,event=>{event.preventDefault();if(busy)return;$('dropzone').classList.toggle('drag',name==='dragover');if(name==='drop'&&event.dataTransfer.files[0])choose(event.dataTransfer.files[0]);});
for(const button of document.querySelectorAll('[data-sample]'))button.addEventListener('click',async()=>{
  setBusy(true);
  try{
    const manifest=await fetch('/samples/manifest.json',{cache:'no-store'}).then(r=>{if(!r.ok)throw new Error('Samples are unavailable.');return r.json();});
    const sample=manifest.cases.find(item=>item.id===button.dataset.sample);
    const response=await fetch(`/samples/${sample.id}.png`);if(!response.ok)throw new Error('The sample image is unavailable.');
    await choose(new File([await response.blob()],`${sample.id}.png`,{type:'image/png'}),sample);
  }catch(failure){error(failure.message);}finally{setBusy(false);}
});

function render(result,status,elapsed,benchmark,deleted){
  lastResult=result;$('result').hidden=false;$('empty').hidden=true;$('result-state').textContent=status.replaceAll('_',' ');
  $('duration').textContent=`${(elapsed/1000).toFixed(1)} s`;
  if(benchmark?.kind==='fields'){$('benchmark').textContent=`${benchmark.correct} / ${benchmark.total}`;$('benchmark-note').textContent='Expected reference fields matched. A sample result is not a guarantee for other images.';}
  else if(benchmark?.kind==='text'){$('benchmark').textContent=`${(benchmark.characterErrorRate*100).toFixed(1)}% CER`;$('benchmark-note').textContent='Character error rate against the sample reference after whitespace normalization. Lower is better.';}
  else if(benchmark?.kind==='blank'){$('benchmark').textContent=benchmark.passed?'No invented text':'Unexpected text';$('benchmark-note').textContent='The reference image contains no readable text.';}
  else{$('benchmark').textContent='Not scored';$('benchmark-note').textContent='Your document has no known reference. We do not infer an accuracy percentage.';}
  cleanupStatus(deleted);
  $('text').textContent=result.text||'No readable text detected.';$('json').textContent=JSON.stringify(result,null,2);
  const warnings=[...(result.warnings??[]),...(result.validation_failures??[])];
  $('warnings').hidden=!warnings.length;$('warnings').textContent=warnings.map(value=>typeof value==='string'?value:JSON.stringify(value)).join('\n');
  const fields=Object.entries(result.fields??{});$('field-count').textContent=`(${fields.length})`;$('fields-section').hidden=!fields.length;$('fields').replaceChildren();
  for(const [key,field] of fields.slice(0,200)){
    const row=document.createElement('tr');
    for(const text of [key,typeof field.value==='string'?field.value:JSON.stringify(field.value),typeof field.confidence==='number'?`${(field.confidence*100).toFixed(1)}%`:'—']){const cell=document.createElement('td');cell.textContent=text;row.append(cell);}
    $('fields').append(row);
  }
  $('result-title').focus();
}
function cleanupStatus(deleted){$('cleanup').textContent=deleted?'✓ Server document copies deleted and absence verified.':'Document deletion pending. Server-side cleanup will retry automatically.';$('cleanup').classList.toggle('pending',!deleted);}

$('run').addEventListener('click',async()=>{
  if(busy||!selected)return;
  const number=++runNumber,file=selected,sample=reference;
  setBusy(true);$('error').hidden=true;$('result').hidden=true;$('empty').hidden=true;$('result-state').textContent='Processing';
  const start=performance.now();let token=null,jobId=null;
  try{
    stage('upload','Creating a limited sandbox session…');
    const session=(await request('/v1/sandbox/sessions',{method:'POST',body:{}})).value;token=session.token;
    const digest=Array.from(new Uint8Array(await crypto.subtle.digest('SHA-256',await file.arrayBuffer())),n=>n.toString(16).padStart(2,'0')).join('');
    const upload=(await request('/v1/ocr/uploads',{token,method:'POST',body:{content_type:file.type,content_length:file.size,sha256:`sha256:${digest}`}})).value;
    const uploadUrl=new URL(upload.upload_url);if(uploadUrl.origin!==location.origin)throw new Error('The sandbox returned an invalid upload destination.');
    stage('upload','Uploading your document…');await request(uploadUrl.pathname,{token,method:'PUT',body:file,headers:upload.required_headers});
    await request(`/v1/ocr/uploads/${upload.upload_id}/complete`,{token,method:'POST',body:{}});
    stage('inspect','Checking file safety and document structure…');
    const inspection=await poll(`/v1/ocr/uploads/${upload.upload_id}`,token,status=>['accepted','rejected','expired'].includes(status));
    if(inspection.status!=='accepted')throw new Error('The document did not pass inspection. Please try another document; cleanup will run automatically.');
    const payload={source:{upload_id:upload.upload_id},document_type:'general',output:{text:true,markdown:false,layout:true,evidence:true},language_hints:['en'],processing_class:'interactive'};
    if($('nutrition').checked)payload.extraction={schema_id:'kora.nutrition_label',schema_version:'1'};
    jobId=(await request('/v1/ocr/jobs',{token,method:'POST',body:payload})).value.job_id;
    stage('ocr','Reading the document with the OCR service…');const state=await poll(`/v1/ocr/jobs/${jobId}`,token,isTerminalJob);
    if(isFailedJob(state.status))throw new Error('OCR could not complete this document. Cleanup continues automatically.');
    const {value:result,response}=await request(`/v1/ocr/jobs/${jobId}/result`,{token});
    const deleted=response.headers.get('x-sandbox-images-deleted')==='true';
    render(result,state.status,performance.now()-start,score(sample?.sha256===digest?sample:null,result),deleted);
    stage('done','OCR finished. Inspect the output below.');
    if(!deleted)void waitForCleanup(token,number);
  }catch(failure){error(failure.message);$('result-state').textContent='Could not complete';$('status').textContent='You can try another document. Cleanup continues independently.';if(token&&jobId)try{await request(`/v1/ocr/jobs/${jobId}/cancel`,{token,method:'POST',body:{}});}catch{/* The durable cleanup worker retries cancellation. */}}
  finally{setBusy(false);}
});

async function waitForCleanup(token,number){
  for(let attempt=0;attempt<12&&number===runNumber;attempt++){
    await pause(10000);
    try{const {value}=await request('/v1/sandbox/session',{token});if(number!==runNumber)return;if(value.images_deleted){cleanupStatus(true);return;}}
    catch{return;}
  }
}
$('copy').addEventListener('click',async()=>{try{await navigator.clipboard.writeText(lastResult?.text??'');$('copy').textContent='Copied';}catch{error('Copy is unavailable in this browser. Select the text to copy it.');}});
$('download').addEventListener('click',()=>{if(!lastResult)return;const url=URL.createObjectURL(new Blob([JSON.stringify(lastResult,null,2)],{type:'application/json'}));const link=document.createElement('a');link.href=url;link.download='ocr-result.json';link.click();setTimeout(()=>URL.revokeObjectURL(url),1000);});
$('curl').textContent=`curl -X POST ${location.origin}/v1/sandbox/sessions\n# Use the returned token as: Authorization: Bearer <token>\n# Then follow /v1/ocr/uploads → /v1/ocr/jobs → /result`;

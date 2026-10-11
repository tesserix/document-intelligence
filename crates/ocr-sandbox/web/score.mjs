const stable = value => JSON.stringify(value && typeof value === 'object' && !Array.isArray(value)
  ? Object.fromEntries(Object.keys(value).sort().map(key => [key, JSON.parse(stable(value[key]))])) : value);
const normalized = text => String(text ?? '').normalize('NFC').trim().replace(/\s+/gu, ' ');

export function score(sample, result) {
  if (!sample) return null;
  const expected = Object.entries(sample.expected_fields ?? {});
  if (expected.length) return {kind:'fields',correct:expected.filter(([key,value]) => stable(result.fields?.[key]?.value) === stable(value)).length,total:expected.length};
  if (sample.reference_text === '') return {kind:'blank',passed:normalized(result.text) === ''};
  if (typeof sample.reference_text !== 'string') return null;
  const a=Array.from(normalized(sample.reference_text)),b=Array.from(normalized(result.text));
  if (a.length > 2000 || b.length > 2000) return null;
  let previous=Array.from({length:b.length+1},(_,i)=>i);
  for (let i=1;i<=a.length;i++) {
    const current=[i];
    for (let j=1;j<=b.length;j++) current[j]=Math.min(current[j-1]+1,previous[j]+1,previous[j-1]+Number(a[i-1]!==b[j-1]));
    previous=current;
  }
  return {kind:'text',characterErrorRate:previous[b.length]/Math.max(1,a.length)};
}

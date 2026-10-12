export async function prepareFile(file) {
  const reject = () => { throw new Error('Choose a PDF, PNG or JPEG file up to 4 MiB.'); };
  if (file.size === 0 || file.size > 4194304) reject();
  const header = new Uint8Array(await file.slice(0, 12).arrayBuffer());
  const starts = bytes => bytes.every((value, index) => header[index] === value);
  const type = starts([137,80,78,71,13,10,26,10]) ? 'image/png'
    : starts([255,216,255]) ? 'image/jpeg'
    : starts([37,80,68,70,45]) ? 'application/pdf' : null;
  if (!type || (file.type && !['application/octet-stream',type,...(type === 'image/jpeg' ? ['image/jpg'] : [])].includes(file.type))) reject();
  return new File([file], file.name, {type, lastModified:file.lastModified});
}

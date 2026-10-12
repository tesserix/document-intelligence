export async function prepareFile(file, maxBytes) {
  if (!Number.isSafeInteger(maxBytes) || maxBytes < 1) throw new Error('Upload limits are unavailable. Refresh the page and try again.');
  const reject = () => { throw new Error(`Choose a PDF, PNG or JPEG file, or a TIFF or WebP image, up to ${maxBytes / 1048576} MiB.`); };
  if (file.size === 0 || file.size > maxBytes) reject();
  const header = new Uint8Array(await file.slice(0, 12).arrayBuffer());
  const starts = bytes => bytes.every((value, index) => header[index] === value);
  const type = starts([137,80,78,71,13,10,26,10]) ? 'image/png'
    : starts([255,216,255]) ? 'image/jpeg'
    : starts([37,80,68,70,45]) ? 'application/pdf'
    : starts([73,73,42,0]) || starts([77,77,0,42]) ? 'image/tiff'
    : starts([82,73,70,70]) && [87,69,66,80].every((value,index)=>header[index+8]===value) ? 'image/webp' : null;
  const aliases = type === 'image/jpeg' ? ['image/jpg'] : type === 'image/tiff' ? ['image/tif','image/x-tiff'] : [];
  if (!type || (file.type && !['application/octet-stream',type,...aliases].includes(file.type))) reject();
  return new File([file], file.name, {type, lastModified:file.lastModified});
}

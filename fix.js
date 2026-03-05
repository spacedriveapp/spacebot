const fs = require('fs');
const content = fs.readFileSync('interface/src/components/MemoryGraph.tsx', 'utf8');

const importsFixed = content.replace(
  'import { HugeiconsIcon } from "@hugeicons/react";',
  'import { HugeiconsIcon } from "@hugeicons/react";\nimport { FontAwesomeIcon } from "@fortawesome/react-fontawesome";\nimport { faPenToSquare, faTrash } from "@fortawesome/free-solid-svg-icons";'
);

const replacedHeader = importsFixed.replace(
  /<div className="mb-2 flex items-center justify-between">([\s\S]*?)<\/Button>\n\s*<\/div>/,
  `<div className="mb-2 flex items-center justify-between">$1</Button>\n\t\t\t\t\t\t\t</div>\n\t\t\t\t\t\t</div>`
).replace(
  /(\s*)<span([\s\S]*?)>([\s\S]*?)<\/span>\n(\s*)<Button[\s\S]*?onClick=\{\(\) => setSelectedNode\(null\)\}[\s\S]*?<\/Button>\n\s*<\/div>/,
  `$1<span$2>$3</span>
$4<div className="flex items-center gap-1">
$4\t<Button
$4\t\tsize="icon"
$4\t\tvariant="outline"
$4\t\tclassName="h-6 w-6 border-transparent bg-transparent hover:border-app-line text-ink-faint flex-shrink-0"
$4\t\ttitle="Edit memory"
$4\t\tonClick={() => {
$4\t\t\tonEditMemory?.(selectedNode.memory);
$4\t\t\tsetSelectedNode(null);
$4\t\t}}
$4\t>
$4\t\t<FontAwesomeIcon icon={faPenToSquare} className="text-[11px]" />
$4\t</Button>
$4\t<Button
$4\t\tsize="icon"
$4\t\tvariant="outline"
$4\t\tclassName="h-6 w-6 border-transparent bg-transparent hover:border-app-line text-ink-faint flex-shrink-0"
$4\t\ttitle="Remove memory"
$4\t\tonClick={() => {
$4\t\t\tonDeleteMemory?.(selectedNode.memory);
$4\t\t\tsetSelectedNode(null);
$4\t\t}}
$4\t>
$4\t\t<FontAwesomeIcon icon={faTrash} className="text-[11px]" />
$4\t</Button>
$4\t<Button
$4\t\tonClick={() => setSelectedNode(null)}
$4\t\tvariant="ghost"
$4\t\tsize="icon"
$4\t\tclassName="ml-1 h-7 w-7"
$4\t>
$4\t\t<HugeiconsIcon icon={Cancel01Icon} className="h-3.5 w-3.5" />
$4\t</Button>
$4</div>
$4</div>`
);

const replacedActions = replacedHeader.replace(
  /\n\s*<div className="mt-3 flex gap-2">[\s\S]*?<\/div>/,
  ''
);

fs.writeFileSync('interface/src/components/MemoryGraph.tsx', replacedActions);

#!/bin/bash
sed -i 's/variant="ghost"/variant="ghost"\n\t\t\t\t\t\t\t\t\tsize="icon"\n\t\t\t\t\t\t\t\t\tclassName="ml-1 h-7 w-7"\n\t\t\t\t\t\t\t\t>\n\t\t\t\t\t\t\t\t\t<HugeiconsIcon icon={Cancel01Icon} className="h-3.5 w-3.5" \/>\n\t\t\t\t\t\t\t\t<\/Button>\n\t\t\t\t\t\t\t<\/div>/g' interface/src/components/MemoryGraph.tsx

SKIPPED: ansible refuses `.md` inside a directory inventory (INVENTORY_IGNORE_EXTS —
measured on 2.21.2, a file here contributed no host). So does `.cfg`, `.bak`, `.retry`,
`.orig`, `.txt`, `.rst`, and anything starting with a dot.

Which is why the picker's numbered list for this folder shows db.ini and hosts.ini and not
this file: that list comes from the same function that reads the folder, so it cannot
advertise a file the reader drops.

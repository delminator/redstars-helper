#!/bin/sh
# Post-remove cleanup.
#
# Retire les liens courts posés par le postinst dans /usr/local/bin
# (redhelper, minitel). dpkg/rpm ne les connaissent pas — c'est à nous de
# les enlever. On ne supprime QUE des liens symboliques qui pointent encore
# vers notre binaire, pour ne jamais toucher un fichier étranger qui
# porterait le même nom.
#
# Ce script tourne aussi lors d'une mise à jour (le postinst du nouveau
# paquet les recrée juste après) : c'est sans conséquence.
#
# Tolérant : toute erreur est ignorée, rien ne bloque la désinstallation.

set +e

BIN="/usr/bin/redstars-helper"
for link in /usr/local/bin/redhelper /usr/local/bin/minitel; do
    if [ -L "$link" ] && [ "$(readlink "$link" 2>/dev/null)" = "$BIN" ]; then
        rm -f "$link" >/dev/null 2>&1
    fi
done

exit 0

# Design do Concord — guia para implementação

Este guia acompanha as telas em `design/telas/`. Implemente a interface (`concord-ui`, Tauri), o menu da bandeja e a janela do visualizador (`concord-core`) **fiéis a essas telas**: layout, cores, textos, estados e acessibilidade.

## O que mudou nesta versão

- O app agora se chama **Concord**. Marca: dois círculos sobrepostos num quadrado de raio 9 em `--accent`.
- **Barra lateral única** (`Lateral.dc.html`) em todas as páginas: Chamada · Amigos · Compartilhar tela · Mesa de sons · Ajustes, presença dos amigos e você no rodapé, com Mutar-se e Silenciar. Por isso saiu o link "Voltar para a chamada".
- **Nova aba Amigos** (saiu de Ajustes) e **novo Visualizador** (janela nativa).
- **Compartilhar tela** revisado: abas Telas/Janelas, resolução 480p–1440p, 30/60 fps, resoluções acima da fonte desabilitadas, variante sem encoder.
- **Ajustes** revisado: medidor com limite arrastável, captura e conflito de atalho, seção Você.
- **Componentes unificados** (`Componentes.dc.html`): botão de texto com 44 px, um só checkbox, um só campo.
- **Tokens:** `--danger` passou de `#E5484D` para `#D13F44` (texto branco agora passa 4,5:1). Novos `--accent-tint` e avatares 3, 4 e 5.
- **Vermelho em erro de formulário:** a regra de cor ganhou essa exceção (veja "Regras de cor").
- **"Identidade verificada"** agora é uma pílula neutra, não verde: não é algo "vivo".
- **Mutar-se e Silenciar** não usam `aria-pressed`: o rótulo muda ("Mutar-se" ↔ "Desmutar-se"), e isso já informa o estado.
- "Modo jogo" saiu da barra de controles da chamada e ficou só no cabeçalho, com o atalho visível.

## Como ler os arquivos `.dc.html`

Cada arquivo é uma tela desenhada num editor de design. **São referência visual e de comportamento, não código para copiar e rodar.**

- O visual está todo em `style="…"` inline: use esses valores (cores, tamanhos, espaçamentos, raios).
- `{{nome}}` é um valor calculado no bloco `<script type="text/x-dc">` no fim do arquivo. Leia esse bloco para entender estados e textos.
- `<sc-for>` repete uma lista, `<sc-if>` mostra um trecho condicional.
- `<dc-import name="X" …>` monta o arquivo `X.dc.html` com os atributos como propriedades. Exemplos: `Main.dc.html` monta a `Lateral`, e `Main-assistindo.dc.html` monta a `Main` com `cenario="assistindo"`.
- Os arquivos com hífen (`Main-assistindo`, `Amigos-erro`…) **são só variantes**: mostram outra tela num estado específico. A lógica de todos os estados está no arquivo base.
- Ignore `<x-dc>`, `<helmet>`, `support.js` e `DCLogic`: são do editor de design.
- `<a href="Outra.dc.html">` indica navegação entre telas.

Reescreva tudo como componentes do frontend escolhido (Svelte ou TypeScript puro), com os tokens abaixo como variáveis CSS.

## Telas

| Arquivo base | Variantes | Onde roda | O que é |
|---|---|---|---|
| `Lateral` | — | `concord-ui` | Barra lateral compartilhada. Propriedades: `ativo`, `presenca`, `transmitindo`, `muted`, `silenced` |
| `Main` | `Main-assistindo`, `Main-transmitindo` | `concord-ui` | Chamada. `cenario`: `amigo` (padrão), `assistindo`, `transmitindo`, `nenhum` |
| `Amigos` | `Amigos-vazio`, `Amigos-erro` | `concord-ui` | Aba Amigos. `cenario`: `padrao`, `vazio`, `carregando`, `erro` |
| `Compartilhar` | `Compartilhar-janelas`, `Compartilhar-sem-encoder` | `concord-ui` | Diálogo. Propriedades: `aba` (`telas`/`janelas`) e `encoder` (`nvenc`/`amf`/`qsv`/`nenhum`) |
| `Ajustes` | — | `concord-ui` | Ajustes. `atalho` mostra os estados de captura: `normal`, `capturando`, `conflito` |
| `Sons` | — | `concord-ui` | Mesa de sons |
| `Visualizador` | `-conectando`, `-encerrada`, `-instavel` | **`concord-core` (wgpu)** | Janela de quem assiste. `estado`: `assistindo`, `conectando`, `encerrada`, `instavel` |
| `ModoJogo` | — | **`concord-core`** | Menu da bandeja e notificação do Windows (APIs nativas). Referência de conteúdo e ordem |
| `Componentes` | — | — | Folha com todos os componentes e estados |

## Tokens

### Cores

```css
:root {
  /* Fundos, do mais escuro ao mais claro */
  --bg:            #0F1012; /* fundo da janela; trilho dos controles segmentados */
  --bg-sunken:     #0B0C0E; /* letterbox do visualizador, miniaturas */
  --surface-nav:   #141619; /* barra lateral e painel de chat */
  --surface-1:     #17191C; /* cartões, seções, diálogos */
  --surface-2:     #1A1C20; /* campos, linhas de lista */
  --surface-3:     #1F2226; /* botão neutro, item ativo do menu, hover do botão de texto */
  --surface-4:     #23262A; /* chips de tecla, trilhos, pílulas neutras */
  --surface-5:     #2A2E33; /* opção selecionada no segmentado, borda de campo */
  --border:        #23262A;

  /* Texto */
  --text:          #EDEEF0;
  --text-muted:    #A0A6AD;
  --text-faint:    #858C94; /* mínimo para texto pequeno; não usar sobre --surface-4 */
  --text-kbd:      #C9CDD2;

  /* Destaque: algo "vivo" (falando, conectado, transmitindo, ação principal) */
  --accent:        #3DD68C;
  --on-accent:     #06281A;
  --accent-soft:   #1E3B33; /* fundos tingidos, pílulas vivas */
  --accent-text:   #8FE8BB; /* texto sobre --accent-soft */
  --accent-tint:   #16241E; /* cartão selecionado ou tocando */

  /* Modo jogo: sempre âmbar, nunca usado para outra coisa */
  --game:          #F2A93B;
  --game-text:     #F5C27A;
  --game-soft:     #2B2214;
  --game-border:   #5A4220;

  /* Perigo: desligar, parar de transmitir, mutado, remover e erro de formulário */
  --danger:        #D13F44; /* fundo; texto branco 4,7:1 */
  --danger-text:   #FF8A8E;
  --danger-soft:   #3A1C1E;

  /* Presença */
  --presence-on:   var(--accent);
  --presence-off:  #4A5057;

  /* Avatares (fundo / letra) */
  --avatar-1-bg: #1E3B33; --avatar-1-fg: #8FE8BB;
  --avatar-2-bg: #352C4A; --avatar-2-fg: #CDBEF5;
  --avatar-3-bg: #213547; --avatar-3-fg: #9CCBF0;
  --avatar-4-bg: #3D2433; --avatar-4-fg: #F2B8D2;
  --avatar-5-bg: #1F3A3D; --avatar-5-fg: #9FE0E6;
  --avatar-off-bg: #2A2E33; --avatar-off-fg: #A0A6AD;
}
```

A cor do avatar é escolhida por amigo (por exemplo, pelo hash do fingerprint) e usada também no nome dele no chat. Offline, o avatar usa `--avatar-off-*`.

### Regras de cor

- **Verde** (`--accent*`): só para algo vivo (falando, na sala, online, conectado, transmitindo) e para a ação principal.
- **Âmbar** (`--game*`): só modo jogo. Única exceção, pedida no briefing: o aviso "Sem codificação na placa de vídeo" usa `--game-text`.
- **Vermelho** (`--danger*`): desligar, parar de transmitir, mutado/silenciado, remover **e erros de validação de formulário** (borda `--danger-text` e mensagem abaixo do campo). Avisos que não são erro do usuário ("Fora da sua rede Tailscale", "Conexão instável", "Tailscale desligado") são **neutros**: ícone de alerta em `--text`, borda `--surface-5`.

### Tipografia

- **Interface:** Manrope (400, 500, 600, 700). Base 14 px.
- **Números, atalhos, IPs, nomes de PC, `.exe`, estatísticas:** JetBrains Mono 500.
- **Empacote as fontes no app** (`.woff2` locais, licença SIL OFL). Nada de Google Fonts em produção.

| Uso | Tamanho | Peso |
|---|---|---|
| Título de tela (h1) | 20–22 px | 700, `letter-spacing: -0.01em` |
| Título de seção (h2) | 16 px | 700 |
| Texto | 14 px | 400–600 |
| Secundário, rótulo de campo | 13 px | 400 |
| Descrição, legenda, chip | 12 px | 400–600 |

Nada abaixo de 12 px.

### Espaçamento e formas

- Espaçamento: 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 24, 28, 32, 40 px.
- Raios: 6 (chip de tecla), 9 (opção do segmentado), 10 (campo, linha compacta, botão de texto, item de menu), 12 (botões, linhas de lista), 14 (banner), 16 (seção), 18 (tile da chamada, barra de controles), 20 (diálogo), 999 (pílula).
- Bordas de 1 px. Sem sombras, gradientes ou desfoque. As únicas "sombras" são anéis sólidos sem desfoque (`0 0 0 Npx`): o anel de quem fala e o halo do ponto "ao vivo".

## Componentes

Todos estão em `Componentes.dc.html`. **Toda área de clique tem no mínimo 44 × 44 px.**

| Componente | Especificação |
|---|---|
| Botão principal | Fundo `--accent`, texto `--on-accent` 700 14 px, altura 44, padding 0 18, raio 12 |
| Botão neutro | Fundo `--surface-3`, texto `--text` 600 |
| Botão de texto | Fundo transparente, texto `--text-muted` 600 13 px, **altura 44**, padding 0 12, raio 10. Hover: fundo `--surface-3`. Variante perigo: texto `--danger-text` ("Remover") |
| Botão perigo | Fundo `--danger`, texto branco 700 ("Desligar", "Parar") |
| Botão de ícone | 44 × 44, raio 10–12, `aria-label` obrigatório |
| Desabilitado | Fundo `--surface-2`, texto `--text-faint`, `cursor: not-allowed`, atributo `disabled` |
| Controle da chamada | 96 × 64, ícone 20 px sobre rótulo 12 px. Ativo (mutado/silenciado): `--danger-soft` + `--danger-text`. Tingido (transmitindo): `--accent-soft` + `--accent-text` |
| Campo | Altura 44, raio 10, fundo `--surface-2`, borda 1 px `--surface-5`, padding 0 12. Rótulo visível acima (13 px `--text-muted`, `<label for>`). Mono para IP. Erro: borda `--danger-text`, mensagem 13 px `--danger-text` com ícone abaixo, `aria-invalid` e `aria-describedby`. **O mesmo componente vale para campos dentro de linhas** (apelido) |
| Seleção | Igual ao campo, com `<select>` nativo |
| Somente leitura | Igual ao campo com borda `--border`, mais um botão de texto ao lado ("Copiar" → "Copiado") |
| Controle segmentado | Trilho `--bg` com padding 4 e raio 12. Opção: altura 44, raio 9. Selecionada: `--surface-5` + `--text`. Normal: transparente + `--text-muted`. Desabilitada: `--text-faint`, `disabled`, e uma legenda explicando o porquê. `role="radiogroup"`/`radio` (ou `tablist`/`tab` para abas) |
| Linha de checkbox | Título 600 (e descrição 12 px opcional) à esquerda, caixa de 20 px à direita, altura mínima 44, o `<label>` inteiro é clicável |
| Linha de lista | Fundo `--surface-2`, raio 12, altura mínima 64, padding 10 12, gap 12: ícone ou avatar 40 · texto (flex) · pílula · ação. **Linha compacta** (jogos): altura 44, raio 10 |
| Pílula de status | Ponto de 6 px + texto 12 px 600, padding 4 10, raio 999. Viva: `--accent-soft`/`--accent-text`. Neutra: `--surface-4`/`--text-kbd` (ou `--text-muted` para offline). Modo jogo: `--game-soft`/`--game-text` |
| Chip de tecla | Mono 12 px, padding 4 8, raio 6, `--surface-4`/`--text-kbd`. Chip de encoder: `--accent-soft`/`--accent-text` |
| Captura de atalho | Botão de 44 px com borda tracejada `--accent` e "Aperte a nova tecla…". Conflito: borda e texto `--danger-text`, mostra a combinação tentada, e a mensagem "Esse atalho já está em uso por X." abaixo, com `role="alert"`. Esc cancela, Tab sai |
| Banner | Fundo `--surface-1`, borda `--border`, raio 14, padding 12 14: ícone 40 em `--accent-soft` · título 600 + linha mono 12 px · ação. Variante "você ao vivo": borda `--accent-soft` e ponto de 10 px com halo de 4 px |
| Aviso neutro | Borda `--surface-5`, raio 10, ícone de alerta em `--text`, título 600 + explicação `--text-muted` |
| Estado vazio | Borda tracejada `--surface-5`, ícone em círculo de 48 (`--surface-3`), título 15 700, frase 13 px `--text-muted`, ação opcional |
| Estado de erro | Como o vazio, mas borda sólida, fundo `--surface-2`, `role="alert"` e ação neutra ("Tentar de novo") |
| Carregando | Linhas esqueleto estáticas (blocos `--surface-4`/`--surface-3`, **sem animação**) e uma frase com `role="status"` |
| Medidor do microfone | Trilho de 10 px `--surface-4`. Barra **sem transição**: `--text-faint` abaixo do limite, `--accent` transmitindo. Marcador do limite = `<input type="range">` nativo por cima (arrastável e por setas), polegar 8 × 24 em `--text` com borda de 3 px na cor do fundo, sem sombra. Estado ao lado do rótulo: "Abaixo do limite" ou "Transmitindo" |
| Avatar | Círculo com a inicial. Quem fala: anel `0 0 0 3px --accent, 0 0 0 9px` com o destaque a 16%. Ponto de presença de 10–11 px com borda de 2 px na cor do fundo |

## Comportamento por tela

### Barra lateral

- Marca, navegação (44 px por item, `aria-current="page"` no ativo), presença e você.
- **A presença fica na lateral** porque com 2 a 4 amigos ela é a informação mais útil fora da chamada, e dá para chamar quem está online com um clique (ícone de telefone, "Chamar X para a sala"). A gestão completa fica em Amigos.
- Estados de presença: **Na sala** (ponto e texto em destaque), **Online** (ponto em destaque, texto `--text-muted`), **Offline** (ponto `--presence-off`, nome e avatar apagados).
- Durante a sua transmissão, o item "Compartilhar tela" mostra "Ao vivo".
- Rodapé: avatar, nome e status do microfone ("Microfone ligado", "Mutado", "Silenciado") com dois botões de ícone 44 × 44. É o mesmo estado da barra de controles da chamada.

### Chamada (Main)

- Cabeçalho: "Chamada com …", tempo em mono, "N na sala" e o botão do modo jogo (âmbar, com o chip `Ctrl+Shift+G`).
- **Amigo transmitindo:** banner "Pedro está transmitindo a tela · 1080p · 60 fps" com **Assistir**. Nada abre sozinho.
- **Assistindo:** o banner vira "Você está assistindo Pedro · em outra janela" com **Mostrar a janela** (texto, traz o visualizador para a frente) e **Parar de assistir** (neutro).
- **Você transmitindo:** banner fixo "Você está transmitindo · N assistindo" com a fonte e o encoder em mono e **Parar** (perigo). O botão "Tela" da barra fica tingido e, clicado, para a transmissão. Seu tile mostra a pílula "Transmitindo".
- Tiles: pílula "Transmitindo" quando for o caso, conexão em mono no canto ("direta · 18 ms" ou "via relay · 64 ms"), avatar, nome, status do microfone e volume individual (0–200%).
- Barra de controles: **Mutar-se/Desmutar-se**, **Silenciar/Voltar a ouvir**, Tela, Sons e Desligar. Silenciar também muta; Desmutar-se enquanto silenciado volta a ouvir.
- Chat: subtítulo obrigatório "Apagado quando a chamada termina". Mensagens de sistema em pílula centralizada ("Pedro começou a transmitir", "Ana entrou para assistir").

### Amigos

- **Seus amigos:** avatar com presença, nome, pílula "Identidade verificada" (neutra), IP em mono, pílula de presença e **Remover**. Amigo fora da tailnet ganha o aviso neutro "Fora da sua rede Tailscale" com a explicação.
- **Na sua rede Tailscale:** nome do PC (mono), dono, IP, online/offline. Quem já é amigo mostra "Já é seu amigo". Os outros têm o campo **Apelido** (sugerido pelo dono) e **Adicionar**. **Atualizar** no cabeçalho, desabilitado enquanto carrega.
- Estados da rede: carregando (esqueleto), vazia ("Nenhum outro dispositivo na sua rede Tailscale"), erro ("Tailscale desligado ou não encontrado" + "Tentar de novo"). Com o Tailscale desligado, todos os amigos aparecem offline.
- **Adicionar pelo IP:** Nome + IP (mono, `100.x.y.z`) + **Adicionar amigo**. Validação ao enviar: nome vazio ("Dê um nome para esse amigo."), IP vazio ("Digite o IP do Tailscale do seu amigo.") e IP fora de `100.x.y.z` ("Esse IP não é da rede Tailscale. Os endereços do Tailscale começam com 100."). Sucesso: mensagem `role="status"`. A faixa exata do Tailscale é `100.64.0.0/10`; valide isso no app.
- **Aba vazia:** um estado vazio grande no lugar de "Seus amigos", explicando que os dois precisam estar na mesma tailnet.

### Compartilhar tela

- Abas **Telas / Janelas** (`tablist`). Cartões de escolha com miniatura, nome e resolução (`radiogroup`). Selecionado: borda 2 px `--accent` + fundo `--accent-tint`. Nas janelas, o quadradinho com a inicial é um marcador para o **ícone real do app** (extraído do `.exe`).
- **Resolução** 480p · 720p · 1080p · 1440p e **Quadros por segundo** 30 · 60.
- Opções maiores que a fonte ficam desabilitadas, com legenda ("Sua tela é 1920 × 1080", "A janela tem 1600 × 900"). Se a opção escolhida ficar inválida ao trocar de fonte, cai para a maior permitida.
- Linha mono "1080p · 60 fps · 8 Mbps" e barra **Uso do seu upload** proporcional. Bitrates (30/60 fps): 480p 1,5/2,5 · 720p 3/5 · 1080p 6/8 · 1440p 10/14 Mbps. Números com vírgula decimal.
- Checkboxes: "Transmitir o áudio do PC" (descrição do briefing) e "Continuar transmitindo no modo jogo".
- Rodapé: chip do encoder detectado + "Codificação na placa de vídeo". **Sem encoder:** aviso em `--game-text` "Sem codificação na placa de vídeo: limite de 720p a 30 fps", com 1080p, 1440p e 60 fps desabilitados.
- **Cancelar** (neutro) e **Começar a transmitir** (principal). Esc fecha. O foco começa na aba ativa e fica preso no diálogo.

### Ajustes

- Duas colunas (uma em janelas estreitas).
- **Áudio:** Microfone, Saída, Volume do microfone (com o valor em %), "Como seu microfone liga" (segmentado), "Tecla para falar" (só em Aperte para falar), Nível do microfone (o limite arrastável só aparece em Ativação por voz), **Ouvir minha voz** ↔ **Parar de ouvir**, Qualidade da voz e Supressão de ruído.
- **Modo jogo** (título em âmbar): Entrar sozinho ao abrir um jogo, lista de jogos em linhas compactas com Remover, **Adicionar jogo** (abre o seletor de arquivos do Windows) e Continuar transmitindo no modo jogo.
- **Atalhos:** chip + **Alterar**. Ao alterar: captura ("Aperte a nova tecla…" + Cancelar) e conflito ("Esse atalho já está em uso por X.").
- **Você:** Seu nome (campo + Salvar → Salvo) e Seu endereço (IP em mono, Copiar → Copiado, pílula "Tailscale conectado").

### Mesa de sons

- Cabeçalho com **Adicionar som** (principal). Uma faixa com o Volume geral (com %) e o checkbox "Ouvir no meu fone também".
- Grade de cartões: nome, duração em mono, chip do atalho, tocar/parar (44 px, círculo) e volume. Tocando: borda `--accent`, fundo `--accent-tint`, botão em destaque. No MVP, um som por vez.
- Último cartão: área tracejada para soltar arquivos (também é um botão que abre o seletor).

### Visualizador (janela nativa, `wgpu`)

- Desenhe só retângulos, texto e ícones simples. O vídeo ocupa o máximo mantendo a proporção. O resto é letterbox em `--bg-sunken`.
- **Barra de controles:** aparece ao mover o mouse e some após 2 s parado (só opacidade, até 150 ms). Contém: avatar e nome de quem transmite com a resolução, volume do áudio da tela, **Tela cheia (F11)**, **Estatísticas (F3)** e **Parar de assistir**. Com o foco do teclado dentro da barra, ela não some.
- **Estatísticas (F3):** caixa no canto superior esquerdo, mono 12 px, fundo `--bg`, borda `--surface-5`: fps, bitrate, perda, latência e resolução.
- **Estados:** "Conectando à transmissão…" (centro, sem animação), "Transmissão encerrada" (cartão com Fechar) e "Conexão instável" (pílula neutra no topo, discreta).
- Esc sai da tela cheia; fora dela, não fecha a janela.

### Bandeja e notificação (ModoJogo)

- Menu: cabeçalho "Concord" + pílula Modo jogo + "Na sala com …"; itens de 44 px: Mutar-se, Silenciar, **Sair do modo jogo** (âmbar), separador, Fechar o Concord. Os rótulos mudam com o estado (Desmutar-se, Voltar a ouvir).
- Notificação nativa do Windows para mensagens no modo jogo: "Abrir o chat" e "Dispensar".

## Acessibilidade (obrigatório)

- Áreas de clique de no mínimo 44 × 44 px.
- Contraste de texto de pelo menos 4,5:1. Nada mais claro que `--text-faint` para texto pequeno, e não use `--text-faint` sobre `--surface-4`.
- Controles reais: `<button>`, `<a>`, `<input>`, `<select>`, `<label>`. Nunca clique em `<div>`.
- `aria-label` em botões só com ícone. `aria-current="page"` no item ativo da lateral. Abas com `tablist`/`tab`/`tabpanel`. Grupos de escolha com `radiogroup`/`radio`.
- Botões cujo rótulo muda com o estado (Mutar-se ↔ Desmutar-se, Silenciar ↔ Voltar a ouvir, Ouvir minha voz ↔ Parar de ouvir) **não** usam `aria-pressed`. Botões com rótulo fixo que ligam e desligam (Estatísticas, Tela transmitindo) usam.
- Erros com `aria-invalid` + `aria-describedby`. Mensagens de status com `role="status"`, erros que surgem com `role="alert"`. O chat é `role="log"`.
- Foco visível: `outline: 2px solid var(--accent); outline-offset: 2px` em `:focus-visible`.
- Tudo operável pelo teclado. Esc fecha o diálogo de compartilhar e cancela a captura de atalho.
- Transições de no máximo 150 ms e só de opacidade ou cor. Sem animações decorativas, sem spinners. Respeite `prefers-reduced-motion`.

## Desempenho da interface

- Sem bibliotecas de UI pesadas, sem animações contínuas e sem polling. A interface só redesenha quando o núcleo envia um evento pelo IPC.
- O anel de quem fala e o medidor do microfone atualizam no máximo a cada ~100 ms.
- A interface é encerrada no modo jogo, mas deve abrir rápido: meta de menos de 1 s até a primeira tela.
- Os nomes (Pedro, Ana, Camila, Bruno, Lucas) e todos os números nas telas são exemplos. Na interface real, tudo vem da configuração e do núcleo.

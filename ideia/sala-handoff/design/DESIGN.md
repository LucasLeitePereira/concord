# Design da Sala — guia para implementação

Este guia acompanha as telas em `design/telas/`. Implemente a interface (`sala-ui`, Tauri) **fiel a essas telas**: layout, cores, textos, estados e acessibilidade.

## Como ler os arquivos `.dc.html`

Cada arquivo é uma tela desenhada num editor de design. **Eles são referência visual, não código para copiar e rodar.**

- O visual está todo em `style="…"` inline: use esses valores exatos (cores, tamanhos, espaçamentos, raios).
- `{{nome}}` é um valor dinâmico calculado no bloco `<script type="text/x-dc">` no fim do arquivo. Leia esse bloco para entender os estados (por exemplo, botão mutado ou não).
- `<sc-for>` é uma repetição (lista), e `<sc-if>` é um trecho condicional.
- Ignore `<x-dc>`, `<helmet>`, `support.js` e `DCLogic`: são do editor de design.
- `<a href="Outra.dc.html">` indica navegação entre telas.

Reescreva tudo como componentes do frontend escolhido (Svelte ou TypeScript puro), com os tokens abaixo como variáveis CSS.

## Telas

| Arquivo | Tela | Observações |
|---|---|---|
| `Main.dc.html` | Janela principal em chamada | Menu lateral, palco com os participantes, barra de controles, chat à direita. Layout fluido |
| `Compartilhar.dc.html` | Diálogo de compartilhar tela | Escolha de tela/janela, qualidade, uso do upload, áudio, encoder detectado |
| `Sons.dc.html` | Mesa de sons | Grade de sons com atalho, tocar/parar e volume; área para soltar arquivos |
| `Ajustes.dc.html` | Ajustes | Áudio, modo jogo, atalhos e amigos |
| `ModoJogo.dc.html` | Bandeja e notificação | **Não faz parte do `sala-ui`.** É o menu da bandeja e a notificação do Windows, ambos feitos pelo `sala-core` com APIs nativas. Use como referência de conteúdo e ordem dos itens |

## Tokens

### Cores

```css
:root {
  /* Fundos, do mais escuro ao mais claro */
  --bg:            #0F1012; /* fundo da janela */
  --bg-sunken:     #0B0C0E; /* miniaturas, áreas rebaixadas */
  --surface-nav:   #141619; /* menu lateral e painel de chat */
  --surface-1:     #17191C; /* cartões e painéis */
  --surface-2:     #1A1C20; /* campos, linhas internas */
  --surface-3:     #1F2226; /* botões neutros, item ativo do menu */
  --surface-4:     #23262A; /* trilhos, chips de atalho, bordas fortes */
  --surface-5:     #2A2E33; /* seleção em controles segmentados, bordas de campo */
  --border:        #23262A;

  /* Texto */
  --text:          #EDEEF0;
  --text-muted:    #A0A6AD;
  --text-faint:    #858C94; /* mínimo permitido para texto pequeno */
  --text-kbd:      #C9CDD2;

  /* Destaque: algo "vivo" (falando, ação principal, conectado) */
  --accent:        #3DD68C;
  --on-accent:     #06281A; /* texto sobre o destaque */
  --accent-soft:   #1E3B33; /* fundos tingidos de destaque */
  --accent-text:   #8FE8BB; /* texto de destaque sobre fundo tingido */

  /* Modo jogo: sempre âmbar, nunca usado para outra coisa */
  --game:          #F2A93B;
  --game-text:     #F5C27A;
  --game-soft:     #2B2214;
  --game-border:   #5A4220;

  /* Perigo: desligar, mutado, remover */
  --danger:        #E5484D;
  --danger-text:   #FF8A8E;
  --danger-soft:   #3A1C1E;

  /* Avatares (fundo / letra) */
  --avatar-1-bg: #1E3B33; --avatar-1-fg: #8FE8BB;
  --avatar-2-bg: #352C4A; --avatar-2-fg: #CDBEF5;
  --avatar-off-bg: #2A2E33; --avatar-off-fg: #A0A6AD;
}
```

A cor de destaque pode virar uma opção nos ajustes no futuro. Alternativas já testadas no design: `#4CC9F0`, `#F2A93B`, `#B79CFF`. Se implementar isso, mantenha `--on-accent` escuro.

### Tipografia

- **Interface:** Manrope (400, 500, 600, 700). Tamanho base 14 px.
- **Números, atalhos, IPs e estatísticas:** JetBrains Mono 500.
- **Empacote as fontes dentro do app** (arquivos `.woff2` locais). Nada de Google Fonts em produção: a especificação proíbe chamadas externas. As duas fontes têm licença SIL Open Font License, que permite distribuição.

| Uso | Tamanho | Peso |
|---|---|---|
| Título de tela (h1) | 20–22 px | 700, `letter-spacing: -0.01em` |
| Título de seção (h2) | 15–16 px | 700 |
| Texto | 14 px | 400–600 |
| Secundário | 13 px | 400 |
| Rótulos e legendas | 12 px | 400–600 |
| Atalhos (mono) | 11–12 px | 500 |

### Espaçamento e formas

- Escala de espaçamento: 2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 24, 28 px.
- Raios: 6 px (chips de atalho), 8–10 px (campos, itens de menu), 12 px (botões), 14–16 px (cartões), 18–20 px (painéis grandes e diálogos), 999 px (pílulas de status).
- Bordas de 1 px em `--border`. Sem sombras, gradientes ou desfoque.

## Componentes recorrentes

- **Botão principal:** fundo `--accent`, texto `--on-accent` 700, altura mínima 44 px, raio 12.
- **Botão neutro:** fundo `--surface-3`, texto `--text`.
- **Botão de controle da chamada:** 84 × 64 px, ícone 20 px em cima, rótulo 12 px embaixo. Estado ativo de mudo/ensurdecer: fundo `--danger-soft`, texto `--danger-text`.
- **Controle segmentado:** trilho `--bg` com padding 4 px; opção selecionada `--surface-5` e `--text`; as outras transparentes e `--text-muted`. Use `role="radiogroup"` / `role="radio"`.
- **Chip de atalho:** mono 11–12 px, fundo `--surface-4`, texto `--text-kbd`, raio 6.
- **Pílula de status:** ponto de 6 px + texto 12–13 px, raio 999.
- **Avatar:** círculo com a inicial. Quem está falando ganha um anel de 3 px em `--accent` mais um halo de 9 px com 16% de opacidade.
- **Ícones:** traço de 1,8–2 px, `stroke-linecap: round`, 16–20 px, cor herdada do texto. Use os SVGs das telas ou uma biblioteca de ícones de traço equivalente (por exemplo, Lucide). Sem emoji.

## Comportamento e estados

- **Mudo e ensurdecer:** ensurdecer também muta o microfone. O status embaixo do seu avatar acompanha ("Microfone ligado" / "Microfone mutado" / "Ensurdecido").
- **Quem está falando:** o anel do avatar acende em tempo real a partir do nível de voz enviado pelo núcleo. Atualize no máximo a cada ~100 ms para não pesar.
- **Transmissão do amigo:** o banner "Pedro está transmitindo a tela" com "Assistir" só aparece enquanto existe transmissão. "Assistir" pede ao núcleo para abrir o visualizador nativo.
- **Qualidade da transmissão:** ao trocar o preset, o detalhe (resolução · fps · Mbps) e a barra de uso do upload atualizam. A barra é proporcional de verdade ao upload medido ou informado.
- **Encoder:** o chip mostra o encoder real detectado pelo núcleo (`NVENC · AV1`, `AMF · HEVC`, `QuickSync · H.264`…). Sem encoder de hardware, mostre o aviso em `--game-text` explicando o limite de 720p30.
- **Mesa de sons:** só um som tocando por vez no MVP. O cartão que toca ganha borda `--accent` e fundo tingido, e o botão vira "parar".
- **Modo de ativação:** a linha "Tecla para falar" só aparece com "Aperte para falar" selecionado.
- **Chat:** o subtítulo "Apagado quando a chamada termina" é obrigatório. Mensagens de sistema ("Pedro começou a transmitir") ficam centralizadas em pílula.
- **Modo jogo:** o botão âmbar da tela principal entra no modo jogo (o núcleo fecha a interface). O menu da bandeja e a notificação seguem `ModoJogo.dc.html`.

## Acessibilidade (obrigatório)

- Áreas de clique de no mínimo 44 × 44 px.
- Contraste de texto de pelo menos 4,5:1. Não use cinza mais claro que `--text-faint` sobre os fundos escuros.
- Controles reais: `<button>`, `<a>`, `<input>`, `<select>`, `<label>`. Nunca clique em `<div>`.
- `aria-label` em botões só com ícone. `aria-pressed` em mudo, ensurdecer e tocar. `aria-current="page"` no item ativo do menu.
- Foco visível: `outline: 2px solid var(--accent); outline-offset: 2px` em `:focus-visible`.
- Tudo operável pelo teclado. Esc fecha o diálogo de compartilhar tela.
- Respeite `prefers-reduced-motion`. A interface não tem animações decorativas; transições, se houver, curtas (até 150 ms) e só de opacidade ou cor.

## Desempenho da interface

- Sem bibliotecas de UI pesadas, sem animações contínuas e sem polling agressivo. A interface só redesenha quando o núcleo envia um evento pelo IPC.
- A interface é encerrada no modo jogo, mas deve abrir rápido: meta de menos de 1 s até a primeira tela.

# Briefing para o Claude Design — Concord (revisão do padrão + compartilhar tela)

> Como usar: abra um projeto no Claude Design, anexe `DESIGN.md` e os 5 arquivos de `telas/` (`Main`, `Ajustes`, `Compartilhar`, `Sons`, `ModoJogo`) e cole o texto abaixo da linha.

---

Você vai revisar e ampliar o design do **Concord**, um app desktop para Windows de voz P2P entre amigos (sobre o Tailscale), com chat temporário, modo jogo e, agora, compartilhamento de tela. O design atual está nos arquivos anexados e as regras estão no `DESIGN.md`. **Mantenha o mesmo visual:** tema escuro, tokens de cor, Manrope e JetBrains Mono, sem sombras, gradientes, desfoque ou emoji, e ícones de traço de 1,8–2 px. Todos os textos da interface ficam em pt-BR.

## Objetivo
1. Deixar o app inteiro num padrão único e coerente, com componentes reutilizáveis (campo, botão principal/neutro/texto, controle segmentado, linha de checkbox, linha de lista, pílula de status, chip, banner, estado vazio).
2. Criar as telas e os estados que ainda não têm design.

## Regras que não podem mudar
- Áreas de clique de **no mínimo 44 × 44 px**, contraste de pelo menos 4,5:1 e nada mais claro que `--text-faint` (#858C94) para texto pequeno.
- Controles reais (`button`, `input`, `select`, `label`), foco visível com `outline: 2px solid` no destaque, tudo operável pelo teclado.
- Transições de no máximo 150 ms e só de opacidade ou cor. Sem animações decorativas.
- Âmbar (`--game*`) é exclusivo do modo jogo. Vermelho (`--danger*`) só para desligar, mutado e remover. O verde de destaque marca algo "vivo" (falando, conectado, transmitindo, ação principal).
- Nomes fixos: **Mutar-se/Desmutar-se**, **Silenciar/Voltar a ouvir**, **Modo jogo**. O subtítulo do chat "Apagado quando a chamada termina" é obrigatório.

## Contradições a resolver no design atual
- Os botões "Alterar", "Remover", "Ouvir minha voz", "+ Adicionar jogo" e "Voltar para a chamada" têm 32–36 px de altura, abaixo da regra dos 44 px. Defina um **botão de texto/terciário** que cumpra os 44 px de área de clique (o visual pode continuar discreto).
- Defina **um único componente de checkbox**: texto e descrição à esquerda, caixa de 20 px à direita e altura mínima de 44 px (como já está em Ajustes/Compartilhar).
- Defina **um único componente de campo** (raio 10, fundo `--surface-2`, borda `--surface-5`, 44 px) também para os campos pequenos dentro de linhas de lista.
- Medidor do microfone: a barra de nível muda de largura em tempo real, sem transição. Mostre também o estado "abaixo do limite" (barra em `--text-faint`) e "transmitindo" (barra em `--accent`), e um marcador de limite arrastável sem sombra.

## Telas

### 1. Main (revisar)
- **Lateral:** marca Concord; itens **Chamada · Amigos · Compartilhar tela · Mesa de sons · Ajustes** (Amigos é novo); abaixo, a presença compacta dos amigos (avatar, nome, "na sala"/"online"/"offline"); no rodapé, você (avatar, nome, status do microfone). Um atalho discreto "Adicionar amigo" leva à aba Amigos. Se achar melhor tirar a presença da lateral, proponha e justifique.
- **Amigo transmitindo (banner no topo do palco):** "Pedro está transmitindo a tela · 1080p 60" com o botão principal **Assistir**. Quem assiste escolhe entrar; nada abre sozinho.
- **Assistindo:** o banner vira "Você está assistindo Pedro" com **Parar de assistir** (neutro). O vídeo abre numa janela separada (tela 6).
- **Você transmitindo:** indicador fixo no topo ou na barra de controles: ponto verde + "Você está transmitindo · 2 assistindo" + **Parar** (perigo). O botão "Tela" da barra de controles fica ativo (tingido de destaque) enquanto você transmite.
- Mensagens de sistema no chat, em pílula centralizada: "Pedro começou a transmitir", "Ana entrou para assistir".

### 2. Amigos (nova, aba própria)
Hoje isso fica dentro de Ajustes e foi feito sem design. Deve conter:
- **Seus amigos:** linha com avatar, nome, IP em mono, pílula "Identidade verificada", estado (na sala / online / offline), aviso **"Fora da sua rede Tailscale"** com ícone de alerta (sem âmbar, que é só do modo jogo) (o amigo precisa estar na mesma tailnet ou com a máquina compartilhada), e o botão de texto **Remover** (perigo).
- **Na sua rede Tailscale:** dispositivos encontrados, cada um com nome do PC, dono, IP mono, ponto online/offline, campo de **apelido** (já vem sugerido) e botão **Adicionar**; os que já são amigos mostram "Já é seu amigo". Botão **Atualizar** no cabeçalho da seção. Estados: carregando, vazio ("Nenhum outro dispositivo na sua rede Tailscale") e erro ("Tailscale desligado ou não encontrado").
- **Adicionar pelo IP:** Nome + IP do Tailscale (100.x.y.z, mono) + **Adicionar amigo**, com mensagem de erro abaixo do campo.
- Estado vazio da aba inteira (nenhum amigo ainda), com uma frase curta explicando o Tailscale.

### 3. Compartilhar tela (revisar o diálogo)
- Abas **Telas / Janelas** com miniaturas (nome, resolução, "principal"). Na aba Janelas, uma grade de janelas com título e ícone do app.
- **Resolução:** controle segmentado **480p · 720p · 1080p · 1440p**.
- **Quadros por segundo:** controle segmentado **30 · 60**.
- Linha de detalhe em mono: "1080p · 60 fps · 8 Mbps". Bitrates: 480p 1,5/2,5 · 720p 3/5 · 1080p 6/8 · 1440p 10/14 Mbps (30/60 fps).
- Barra **Uso do seu upload**, proporcional ao bitrate escolhido.
- Resoluções maiores que a tela ou janela escolhida ficam desabilitadas, com legenda ("Sua tela é 1920 × 1080").
- **Transmitir o áudio do PC** (checkbox, descrição: "Quem assiste ouve o som do jogo ou do vídeo. Sua voz e a dos amigos não entram.").
- **Continuar transmitindo no modo jogo** (checkbox).
- Rodapé: chip do encoder detectado (`NVENC · AV1`, `AMF · HEVC`, `QuickSync · H.264`) + "Codificação na placa de vídeo". **Variante sem encoder de hardware:** aviso em `--game-text` "Sem codificação na placa de vídeo: limite de 720p a 30 fps" e as opções acima disso desabilitadas.
- Botões **Cancelar** (neutro) e **Começar a transmitir** (principal). Esc fecha.

### 4. Ajustes (revisar)
Sem a seção Amigos. Grade de 2 colunas:
- **Áudio:** Microfone, Saída, Volume do microfone (com o valor em %), Nível do microfone com o limite de sensibilidade arrastável (só no modo "Ativação por voz"), **Ouvir minha voz**, "Como seu microfone liga" (segmentado), "Tecla para falar" (só em "Aperte para falar"), Qualidade da voz e Supressão de ruído.
- **Modo jogo** (em âmbar): Entrar sozinho ao abrir um jogo, lista de jogos com remover e **+ Adicionar jogo**, Continuar transmitindo no modo jogo.
- **Atalhos:** lista com chip da tecla e **Alterar**, incluindo o estado "Aperte a nova tecla…" e o erro "Esse atalho já está em uso".
- **Você:** Seu nome (campo + Salvar) e Seu endereço (IP do Tailscale em mono, com **Copiar** e o estado do Tailscale).

### 5. Sons e ModoJogo
Apenas alinhe ao padrão novo (botões, chips, cartões). `ModoJogo` continua sendo só referência do menu da bandeja e da notificação do Windows.

### 6. Visualizador (nova; janela nativa separada)
Janela desenhada pelo próprio núcleo (wgpu, não HTML), então use só retângulos, texto e ícones simples.
- O vídeo ocupa tudo, com fundo `--bg-sunken` nas bordas (letterbox).
- Uma barra pequena aparece ao mover o mouse e some após 2 s parado: nome de quem transmite, volume do áudio da tela (slider), **Tela cheia (F11)**, **Estatísticas (F3)** e **Parar de assistir**.
- Sobreposição de estatísticas (F3), em mono, num canto: fps, bitrate, perda, latência e resolução.
- Estados: "Conectando à transmissão…", "Transmissão encerrada" (com Fechar) e "Conexão instável" (aviso discreto).

## Entrega
- Exporte as telas como `.dc.html` com os mesmos nomes (`Main`, `Amigos`, `Compartilhar`, `Ajustes`, `Sons`, `ModoJogo`, `Visualizador`), cobrindo os estados acima (variantes no bloco de lógica ou telas separadas).
- Se mudar algum token, componente ou regra, atualize o `DESIGN.md` junto.
- Os arquivos vão substituir os atuais em `ideia/sala-handoff/design/telas/`.

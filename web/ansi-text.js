/**
 * Linear, forward-only terminal-control scanner for structured log text.
 * This is plain-text extraction, not a terminal emulator. OSC/DCS/SOS/PM/APC
 * payloads, including unterminated strings, are discarded; only xterm renders
 * their terminal semantics. No regex searches rescan attacker-controlled tails.
 */
export function stripAnsi(text) {
  const output=[];let state='text',osc=false,index=0;
  while(index<text.length){
    let code=text.charCodeAt(index);
    if(state==='text'){
      const start=index;
      while(index<text.length){
        code=text.charCodeAt(index);
        if(code===9||code===10||(code>=32&&(code<127||code>159)))index++;
        else break;
      }
      if(index>start){output.push(text.slice(start,index));continue;}
      if(code===27)state='escape';
      else if(code===155)state='csi';
      else if([144,152,157,158,159].includes(code)){state='string';osc=code===157;}
      else if(code===13){output.push('\n');if(text.charCodeAt(index+1)===10)index++;}
      index++;continue;
    }
    // CAN and SUB cancel an incomplete control sequence.
    if(code===24||code===26){state='text';index++;continue;}
    if(state==='string'){
      if(code===156||(osc&&code===7)){state='text';}
      else if(code===27&&text.charCodeAt(index+1)===92){state='text';index++;}
      index++;continue;
    }
    if(code===27){state='escape';index++;continue;}
    if(code===9||code===10){output.push(text[index]);index++;continue;}
    if(code===13){output.push('\n');if(text.charCodeAt(index+1)===10)index++;index++;continue;}
    if(state==='escape'){
      if(code===91)state='csi';
      else if([80,88,93,94,95].includes(code)){state='string';osc=code===93;}
      else if(code>=32&&code<=47)state='intermediate';
      else if(code>=48&&code<=126)state='text';
      else if(code>=160){state='text';continue;}
    }else if(state==='intermediate'){
      if(code>=48&&code<=126)state='text';
      else if(code>=160){state='text';continue;}
    }else if(state==='csi'){
      if(code>=64&&code<=126)state='text';
      else if(code>=160){state='text';continue;}
    }
    index++;
  }
  return output.join('');
}

// The host swaps this class in for `Shell.MenuOptions`. It turns Super's last row into ours and adds
// a fresh Back below it. Only one choice may answer Escape, so the old row loses `bCancel`.
class ModernMenuOptions extends MenuOptions;

var ShellMenuChoice     ModernChoice;
var localized string    ModernText;
var localized string    ModernHelp;

function CreateMenuContents()
{
	Super.CreateMenuContents();

	ModernChoice = BackChoice;
	ModernChoice.SetText(ModernText);
	ModernChoice.SetHelpText(ModernHelp);
	ModernChoice.bCancel = false;

	BackChoice = AddChoice(BackText, "", ItemFont, ItemAlign, true);
}

// Super first: it matches the current BackChoice (the new one), so Back and
// Escape still go back; our row is the old object and falls through to here.
function Notify(UWindowDialogControl C, byte E)
{
	Super.Notify(C, E);

	if (E == DE_Click && C != None && C == ModernChoice)
		GoToMenu(class'ModernMenuGraphics');
}

defaultproperties
{
	ModernText="Modern Graphics"
	ModernHelp="Shadows, ambient occlusion, surface relief and other new graphics, each one on or off."
}

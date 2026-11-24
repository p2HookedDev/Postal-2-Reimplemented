// No preset writes these rows, and changing one keeps the preset.
class ModernMenuScreen extends ModernMenuPage;

var localized string    HudNames[3];

function CreateMenuContents()
{
	local UWindowComboControl Combo;
	local int i;

	Super.CreateMenuContents();

	AddTitle(TitleText, TitleFont, TitleAlign);

	bUpdate = false;
	Combo = AddLevelCombo(0, "ModernSharpHud", 1, 2);
	for (i = 0; i < ArrayCount(HudNames); i++)
		Combo.AddItem(HudNames[i]);
	AddBox(1, "ModernWideView");
	AddBox(2, "ModernHudSafeArea");
	AddLevelSlider(3, "ModernPanini", 0, 10, 0);
	AddLevelSlider(4, "ModernFadeIn", 0, 10, 5);
	AddRow(AddCheckbox(Labels[5], Helps[5], ItemFont), "ModernTemporalAA", 0, 1);
	AddBack();
	LoadRows();
}

function RowChanged(int i)
{
}

defaultproperties
{
	TitleText="Screen and HUD"
	BackHelp="Return to the Modern Graphics menu."

	HudNames(0)="Original"
	HudNames(1)="Whole pixels"
	HudNames(2)="Sharp text"

	Labels(0)="Sharp HUD"
	Labels(1)="Wide view"
	Labels(2)="HUD in centre"
	Labels(3)="Edge correction"
	Labels(4)="Fade in"
	Labels(5)="Temporal smoothing"

	Helps(0)="Original scales the HUD as the game did. Whole pixels keeps it crisp; Sharp text also sharpens the writing."
	Helps(1)="On a wide screen the view widens instead of being cut off at the top and bottom."
	Helps(2)="Keeps the HUD inside a 16:9 area in the middle of a wide screen."
	Helps(3)="Undoes the stretching at the edges of a wide field of view. 0 turns it off."
	Helps(4)="Distant things fade in smoothly instead of popping into view. 0 turns it off."
	Helps(5)="Smooths edges over several frames in place of the edge smoothing row. Can blur movement."
}
